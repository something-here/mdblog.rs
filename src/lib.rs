//! static site generator from markdown files.

#![doc(html_logo_url = "https://www.rust-lang.org/logos/rust-logo-128x128-blk-v2.png",
       html_favicon_url = "https://www.rust-lang.org/favicon.ico",
       html_root_url = "https://docs.rs/mdblog")]

extern crate chrono;
extern crate config;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate log;
extern crate hyper;
extern crate futures;
extern crate pulldown_cmark;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate toml;
extern crate tera;
extern crate walkdir;
extern crate open;
extern crate notify;
extern crate glob;
extern crate mime_guess;
extern crate shellexpand;
extern crate percent_encoding;

mod errors;
mod settings;
mod post;
mod theme;
mod utils;
mod service;

use std::thread;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::sync::mpsc::channel;

use glob::Pattern;
use hyper::server::Http;
use tera::{Context, Tera};
use walkdir::{DirEntry, WalkDir};
use serde_json::{Map, Value};
use chrono::Local;
use notify::{DebouncedEvent, RecursiveMode, Watcher, watcher};

use config::Config;
pub use errors::{Error, Result};
pub use settings::Settings;
pub use theme::Theme;
pub use post::Post;
use service::HttpService;
pub use utils::{create_file, log_error};


/// blog object
pub struct Mdblog {
    /// blog root path
    root: PathBuf,
    /// blog settings
    settings: Settings,
    /// blog theme
    theme: Theme,
    /// blog render
    renderer: Tera,
    /// collection of blog posts
    posts: Vec<Rc<Post>>,
    /// tagged posts
    tags: BTreeMap<String, Vec<Rc<Post>>>,
}

impl Mdblog {
    /// create Mdblog from the `root` path
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Mdblog> {
        let root = root.as_ref();
        let settings: Settings = Default::default();
        let theme = Mdblog::get_theme(root, &settings.theme)?;
        let renderer = Mdblog::get_renderer(root, &settings.theme)?;
        Ok(Mdblog {
            root: root.to_owned(),
            settings: settings,
            theme: theme,
            renderer: renderer,
            posts: Vec::new(),
            tags: BTreeMap::new(),
        })
    }

    /// load customize settings
    ///
    /// layered configuration system:
    /// * default settings
    /// * `Config.toml`
    /// * `BLOG_` prefix environment variable
    pub fn load_customize_settings(&mut self) -> Result<()> {
        let mut settings = Config::new();
        settings.merge(self.settings.clone())?;
        settings.merge(config::File::with_name("Config.toml"))?;
        settings.merge(config::Environment::with_prefix("BLOG"))?;
        self.settings = settings.try_into()?;
        self.renderer = Mdblog::get_renderer(&self.root, &self.settings.theme)?;
        Ok(())
    }

    /// get theme
    pub fn get_theme<P: AsRef<Path>>(root: P, name: &str) -> Result<Theme> {
        let mut theme = Theme::new(root.as_ref());
        theme.load(name)?;
        Ok(theme)
    }

    pub fn get_renderer<P: AsRef<Path>>(root: P, theme_name: &str) -> Result<Tera> {
        let template_dir = root.as_ref()
                               .join("_themes")
                               .join(theme_name)
                               .join("templates");
        debug!("template dir: {}", template_dir.display());
        let renderer = Tera::new(&format!("{}/*", template_dir.display()))?;
        Ok(renderer)
    }

    pub fn load(&mut self) -> Result<()> {
        let mut posts: Vec<Rc<Post>> = Vec::new();
        let mut tags: BTreeMap<String, Vec<Rc<Post>>> = BTreeMap::new();
        let posts_dir = self.root.join("posts");
        let walker = WalkDir::new(&posts_dir).into_iter();

        for entry in walker.filter_entry(|e| !is_hidden(e)) {
            let entry = entry.expect("get walker entry error");
            if !is_markdown_file(&entry) {
                continue;
            }
            let mut post = Post::new(&self.root,
                                     &entry.path()
                                           .strip_prefix(&self.root)
                                           .expect("create post path error")
                                           .to_owned());
            post.load()?;
            let post = Rc::new(post);
            posts.push(post.clone());
            if !post.is_hidden() {
                for tag in post.tags() {
                    let mut ps = tags.entry(tag.to_string()).or_insert(Vec::new());
                    ps.push(post.clone());
                }
            }
        }
        posts.sort_by(|p1, p2| p2.datetime().cmp(&p1.datetime()));
        for (_, tag_posts) in tags.iter_mut() {
            tag_posts.sort_by(|p1, p2| p2.datetime().cmp(&p1.datetime()));
        }
        self.posts = posts;
        self.tags = tags;
        Ok(())
    }

    /// init Mdblog with `theme`.
    ///
    /// theme directory is created at `root/_theme` directory.
    /// if `theme` is `None`, use the default theme(`simple`).
    pub fn init(&mut self) -> Result<()> {
        if self.root.exists() {
            return Err(Error::RootDirExisted(self.root.clone()));
        }

        let mut hello_post = create_file(&self.root.join("posts").join("hello.md"))?;
        hello_post.write_all(HELLO_POST)?;
        let mut math_post = create_file(&self.root.join("posts").join("math.md"))?;
        math_post.write_all(MATH_POST)?;

        self.export_config()?;

        self.theme.load(&self.settings.theme)?;
        self.theme.init_dir(&self.theme.name)?;
        std::fs::create_dir_all(self.root.join("media"))?;
        Ok(())
    }

    /// create the blog html files to `root/_build/` directory.
    ///
    /// if `theme` is `None`, use the default theme(`simple`).
    pub fn build(&mut self) -> Result<()> {
        self.export()?;
        Ok(())
    }

    /// serve the blog static files built in `root/_build/` directory.
    pub fn serve(&mut self, port: u16) -> Result<()> {
        let addr_str = format!("127.0.0.1:{}", port);
        let server_url = format!("http://{}", &addr_str);
        let addr = addr_str.parse()?;
        let build_dir = self.get_build_dir()?;
        info!("server blog at {}", server_url);

        let child = thread::spawn(move || {
            let server = Http::new()
                .bind(&addr, move || Ok(HttpService{root: build_dir.clone()}))
                .expect("server start error");
            server.run().unwrap();
        });

        open::that(server_url)?;
        self.watch()?;
        child.join().expect("Couldn't join the server thread");

        Ok(())
    }

    fn watch(&mut self) -> Result<()> {
        let (tx, rx) = channel();
        let ignore_patterns = self.get_ignore_patterns()?;
        info!("watching dir: {}", self.root.display());
        let mut watcher = watcher(tx, Duration::new(2, 0))?;
        watcher.watch(&self.root, RecursiveMode::Recursive)?;
        let interval = Duration::new(self.settings.rebuild_interval as u64, 0);
        let mut last_run: Option<Instant> = None;
        loop {
            match rx.recv() {
                Err(why) => error!("watch error: {:?}", why),
                Ok(event) => {
                    match event {
                        DebouncedEvent::Create(ref fpath) |
                        DebouncedEvent::Write(ref fpath)  |
                        DebouncedEvent::Remove(ref fpath) |
                        DebouncedEvent::Rename(ref fpath, _) => {
                            if ignore_patterns.iter().any(|ref pat| pat.matches_path(fpath)) {
                                continue;
                            }
                            let now = Instant::now();
                            if let Some(last_time) = last_run {
                                if now.duration_since(last_time) < interval {
                                    continue;
                                }
                            }
                            last_run = Some(now);
                            info!("Modified file: {}", fpath.display());
                            info!("Rebuild blog again...");
                            if let Err(ref e) = self.load() {
                                log_error(e);
                                continue
                            }
                            if let Err(ref e) = self.build() {
                                log_error(e);
                                continue
                            }
                            info!("Rebuild done!");
                        },
                        _ => {},
                    }
                },
            }
        }
        #[allow(unreachable_code)]
        Ok(())
    }

    fn get_build_dir(&self) -> Result<PathBuf> {
        let expanded_path = shellexpand::full(&self.settings.build_dir)?.into_owned();
        let build_dir = PathBuf::from(expanded_path.to_string());
        if build_dir.is_relative() {
            return Ok(self.root.join(&build_dir));
        } else {
            return Ok(build_dir);
        }
    }

    fn get_ignore_patterns(&self) -> Result<Vec<Pattern>> {
        let mut patterns = vec![Pattern::new("**/.*")?];
        let build_dir = self.get_build_dir()?
                            .to_str()
                            .expect("get build dir error")
                            .to_string();
        patterns.push(Pattern::new(&format!("{}/**/*", build_dir.trim_right_matches("/")))?);
        Ok(patterns)
    }

    pub fn create_post(&self, path: &Path, tags: &Vec<String>) -> Result<()> {
        let post_title = path.file_stem();
        let ignore_patterns = self.get_ignore_patterns()?;
        if !path.is_relative()
            || path.extension().is_some()
            || path.to_str().unwrap_or("").is_empty()
            || post_title.is_none()
            || ignore_patterns.iter().any(|ref pat| pat.matches_path(path)) {
            return Err(Error::PostPathInvaild(path.to_owned()));
        }
        if path.is_dir() {
            return Err(Error::PostPathExisted(path.to_owned()));
        }
        let post_path = self.root.join("posts").join(path).with_extension("md");
        if post_path.exists() {
            return Err(Error::PostPathExisted(path.to_owned()));
        }
        let now = Local::now();
        let mut post = create_file(&post_path)?;
        let content = format!("date: {}\n\
                               tags: {}\n\
                               \n\
                               this is a new post!\n",
                              now.format("%Y-%m-%d %H:%M:%S").to_string(),
                              tags.join(", "));
        post.write_all(content.as_bytes())?;
        Ok(())
    }

    pub fn export(&self) -> Result<()> {
        self.export_media()?;
        self.export_static()?;
        self.export_posts()?;
        self.export_index()?;
        self.export_tags()?;
        Ok(())
    }

    pub fn export_config(&self) -> Result<()> {
        let content = toml::to_string(&self.settings)?;
        let mut config_file = create_file(&self.root.join("Config.toml"))?;
        config_file.write_all(content.as_bytes())?;
        Ok(())
    }

    pub fn media_dest<P: AsRef<Path>>(&self, media: P) -> Result<PathBuf> {
        let build_dir = self.get_build_dir()?;
        let rel_path = media.as_ref()
                            .strip_prefix(&self.root.join("media"))?
                            .to_owned();
        Ok(build_dir.join(rel_path))
    }

    pub fn export_media(&self) -> Result<()> {
        debug!("exporting media ...");
        let walker = WalkDir::new(&self.root.join("media")).into_iter();
        for entry in walker.filter_entry(|e| !is_hidden(e)) {
            let entry = entry.expect("get walker entry error");
            let src_path = entry.path();
            if src_path.is_dir() {
                std::fs::create_dir_all(self.media_dest(src_path)?)?;
                continue;
            }
            std::fs::copy(src_path, self.media_dest(src_path)?)?;
        }
        Ok(())
    }

    pub fn export_static(&self) -> Result<()> {
        let build_dir = self.get_build_dir()?;
        self.theme.export_static(&build_dir)?;
        Ok(())
    }

    pub fn export_posts(&self) -> Result<()> {
        let build_dir = self.get_build_dir()?;
        for post in &self.posts {
            let dest = build_dir.join(post.dest());
            let mut f = create_file(&dest)?;
            let html = self.render_post(post)?;
            f.write(html.as_bytes())?;
        }
        Ok(())
    }

    pub fn export_index(&self) -> Result<()> {
        let build_dir = self.get_build_dir()?;
        let dest = build_dir.join("index.html");
        let mut f = create_file(&dest)?;
        let html = self.render_index()?;
        f.write(html.as_bytes())?;
        Ok(())
    }

    pub fn export_tags(&self) -> Result<()> {
        let build_dir = self.get_build_dir()?;
        for tag in self.tags.keys() {
            let dest = build_dir.join(format!("blog/tags/{}.html", tag));
            let mut f = create_file(&dest)?;
            let html = self.render_tag(tag)?;
            f.write(html.as_bytes())?;
        }
        Ok(())
    }

    fn tag_url(&self, name: &str) -> String {
        format!("/blog/tags/{}.html", &name)
    }

    fn tag_map<T>(&self, name: &str, posts: &Vec<T>) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("name".to_string(), Value::String(name.to_string()));
        let tag_len = format!("{:?}", &posts.len());
        map.insert("num".to_string(), Value::String(tag_len));
        map.insert("url".to_string(), Value::String(self.tag_url(&name)));
        map
    }

    pub fn get_base_context(&self, title: &str) -> Result<Context> {
        let mut context = Context::new();
        context.add("title", &title);
        context.add("site_logo", &self.settings.site_logo);
        context.add("site_name", &self.settings.site_name);
        context.add("site_motto", &self.settings.site_motto);
        context.add("footer_note", &self.settings.footer_note);
        let mut all_tags = Vec::new();
        for (tag_key, tag_posts) in &self.tags {
            all_tags.push(self.tag_map(&tag_key, &tag_posts));
        }
        all_tags.sort_by(|a, b| {
                             a.get("name").unwrap()
                              .as_str()
                              .expect("get name error")
                              .to_lowercase()
                              .cmp(&b.get("name")
                                     .expect("get name error")
                                     .as_str()
                                     .expect("get name error")
                                     .to_lowercase())
                         });
        context.add("all_tags", &all_tags);
        Ok(context)
    }

    pub fn render_post(&self, post: &Post) -> Result<String> {
        debug!("rendering post({}) ...", post.path.display());
        let mut context = self.get_base_context(&post.title())?;
        context.add("content", &post.content());
        let mut post_tags = Vec::new();
        if !post.is_hidden() {
            context.add("datetime",
                        &post.datetime().format("%Y-%m-%d %H:%M:%S").to_string());
            for tag_key in post.tags() {
                let tag_posts = self.tags.get(tag_key)
                                    .expect(&format!("post tag({}) does not add to blog tags",
                                                     tag_key));
                post_tags.push(self.tag_map(&tag_key, &tag_posts));
            }
        } else {
            context.add("datetime", &"".to_string());
        }

        context.add("post_tags", &post_tags);
        Ok(self.renderer.render("post.tpl", &context)?)
    }

    pub fn render_index(&self) -> Result<String> {
        debug!("rendering index ...");
        let mut context = self.get_base_context(&self.settings.site_name)?;
        context.add("posts", &self.get_posts_maps(&self.posts)?);
        Ok(self.renderer.render("index.tpl", &context)?)
    }

    fn get_posts_maps(&self, posts: &Vec<Rc<Post>>) -> Result<Vec<Map<String, Value>>> {
        let mut maps = Vec::new();
        for post in posts.iter().filter(|p| !p.is_hidden()) {
            maps.push(post.map());
        }
        Ok(maps)
    }

    pub fn render_tag(&self, tag: &str) -> Result<String> {
        debug!("rendering tag({}) ...", tag);
        let mut context = self.get_base_context(&tag)?;
        let posts = self.tags
                        .get(tag)
                        .expect(&format!("get tag({}) error", &tag));
        context.add("posts", &self.get_posts_maps(&posts)?);
        Ok(self.renderer.render("tag.tpl", &context)?)
    }

    pub fn list_blog_theme(&self) -> Result<()> {
        let theme_root = self.root.join("_themes");
        if !theme_root.exists() || !theme_root.is_dir() {
            error!("no theme");
        }
        for entry in std::fs::read_dir(theme_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                info!("* {}", path.file_name()
                                  .expect("theme name error")
                                  .to_str()
                                  .expect("theme name error"));
            }
        }
        Ok(())
    }

    pub fn create_blog_theme(&self, name: &str) -> Result<()> {
        self.theme.init_dir(name)?;
        Ok(())
    }

    pub fn delete_blog_theme(&self, name: &str) -> Result<()> {
        if self.settings.theme == name {
            return Err(Error::ThemeInUse(name.to_string()));
        }
        let theme_path = self.root.join("_themes").join(name);
        if !theme_path.exists() || !theme_path.is_dir() {
            return Err(Error::ThemeNotFound(name.to_string()));
        }
        std::fs::remove_dir_all(theme_path)?;
        Ok(())
    }

    pub fn set_blog_theme(&mut self, name: &str) -> Result<()> {
        let theme_path = self.root.join("_themes").join(name);
        if !theme_path.exists() || !theme_path.is_dir() {
            return Err(Error::ThemeNotFound(name.to_string()));
        }
        self.settings.theme = name.to_string();
        self.export_config()?;
        Ok(())
    }
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry.file_name()
         .to_str()
         .map(|s| s.starts_with("."))
         .unwrap_or(false)
}

fn is_markdown_file(entry: &DirEntry) -> bool {
    if !entry.path().is_file() {
        return false;
    }
    let fname = entry.file_name().to_str();
    match fname {
        None => {
            return false;
        },
        Some(s) => {
            if s.starts_with(|c| (c == '.') | (c == '~')) {
                return false;
            } else if s.ends_with(".md") {
                return true;
            } else {
                return false;
            }
        },
    }
}

static HELLO_POST: &'static [u8] = include_bytes!("post/hello.md");
static MATH_POST: &'static [u8] = include_bytes!("post/math.md");
