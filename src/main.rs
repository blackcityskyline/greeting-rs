//! greeting — terminal greeter
//!
//! USAGE:
//!   greeting           show greeting (respects cooldown)
//!   greeting show      show greeting (ignore cooldown)
//!   greeting daemon    run as background daemon
//!   greeting update    collect data once and update cache
//!   greeting status    show current state
//!   greeting init      create default config file

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::Deserialize;

// ── Config ────────────────────────────────────────────────────────────────────
//
// All fields are Option<T> so serde can tell "user set this" from "use default".
// Resolved into a flat Config struct after loading.

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    cache:     CacheRaw,
    display:   DisplayRaw,
    colors:    ColorsRaw,
    templates: TemplatesRaw,
    updates:   UpdatesRaw,
    commands:  CommandsRaw,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct CacheRaw {
    /// Minimum seconds between greeting displays. 0 = always show.
    show_cooldown:   Option<u64>,
    /// How often the daemon re-collects system data (seconds).
    update_interval: Option<u64>,
    /// Override cache directory. Default: $XDG_CACHE_HOME/greeting
    dir:             Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DisplayRaw {
    date_format: Option<String>,
    time_format: Option<String>,
    /// Language code. See `greeting init` for supported values.
    lang:        Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ColorsRaw {
    /// Set to false to disable all ANSI colors.
    enabled: Option<bool>,
    green:   Option<String>,
    yellow:  Option<String>,
    purple:  Option<String>,
    red:     Option<String>,
    reset:   Option<String>,
    bold:    Option<String>,
    dim:     Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct TemplatesRaw {
    /// Line 1. Variables: {user} {time_of_day} {date} {time}
    greeting:     Option<String>,
    /// Line 2 — single custom template. Replaces updates/errors parts if set.
    /// Variables: {updates} {errors} + all color vars.
    status_line:  Option<String>,
    updates_none: Option<String>,
    updates_some: Option<String>,
    errors_none:  Option<String>,
    errors_some:  Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UpdatesRaw {
    /// Package manager: pacman | apt | dnf | brew | zypper | flatpak | nix | custom
    manager: Option<String>,
    /// Shell command used when manager = "custom". Must print one integer.
    command: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct CommandsRaw {
    /// Override the journal errors command. Must print one integer.
    errors: Option<String>,
}

// ── Resolved config ───────────────────────────────────────────────────────────

struct Config {
    show_cooldown:   u64,
    update_interval: u64,
    cache_dir:       Option<String>,
    date_format:     String,
    time_format:     String,
    lang:            String,
    // colors (empty string = disabled)
    c_green:  String, c_yellow: String, c_purple: String,
    c_red:    String, c_reset:  String, c_bold:   String, c_dim: String,
    // templates
    tpl_greeting:     String,
    tpl_status_line:  Option<String>,
    tpl_updates_none: String,
    tpl_updates_some: String,
    tpl_errors_none:  String,
    tpl_errors_some:  String,
    // commands
    cmd_updates: String,
    cmd_errors:  String,
}

impl Config {
    fn resolve(r: RawConfig) -> Self {
        let colors_on = r.colors.enabled.unwrap_or(true);

        let color = |opt: Option<String>, default: &str| -> String {
            if !colors_on { return String::new(); }
            opt.unwrap_or_else(|| default.into())
        };

        let manager = r.updates.manager.as_deref().unwrap_or("pacman");
        let custom  = r.updates.command.as_deref().unwrap_or("");

        Config {
            show_cooldown:   r.cache.show_cooldown.unwrap_or(14400),
            update_interval: r.cache.update_interval.unwrap_or(28800),
            cache_dir:       r.cache.dir,
            date_format:     r.display.date_format.unwrap_or_else(|| "%b %d, %a".into()),
            time_format:     r.display.time_format.unwrap_or_else(|| "%H:%M".into()),
            lang:            r.display.lang.unwrap_or_else(|| "en".into()),
            c_green:  color(r.colors.green,  "\x1b[32m"),
            c_yellow: color(r.colors.yellow, "\x1b[33m"),
            c_purple: color(r.colors.purple, "\x1b[38;5;147m"),
            c_red:    color(r.colors.red,    "\x1b[31m"),
            c_reset:  color(r.colors.reset,  "\x1b[0m"),
            c_bold:   color(r.colors.bold,   "\x1b[1m"),
            c_dim:    color(r.colors.dim,    "\x1b[2m"),
            tpl_greeting: r.templates.greeting.unwrap_or_else(|| concat!(
                "Welcome, {c_green}{user}{c_reset}.",
                " Good {c_purple}{time_of_day}{c_reset}.",
                " Date: {c_yellow}{date}{c_reset}",
                " / Time: {c_green}{time}{c_reset}",
            ).into()),
            tpl_status_line:  r.templates.status_line,
            tpl_updates_none: r.templates.updates_none.unwrap_or_else(||
                "Fresh as ever — {c_purple}no updates{c_reset}".into()),
            tpl_updates_some: r.templates.updates_some.unwrap_or_else(||
                "A fresh batch — {c_green}{updates} updates{c_reset}".into()),
            tpl_errors_none: r.templates.errors_none.unwrap_or_else(||
                "Errors: {c_green}{errors}{c_reset}".into()),
            tpl_errors_some: r.templates.errors_some.unwrap_or_else(||
                "Errors: {c_red}{errors}{c_reset}".into()),
            cmd_updates: updates_cmd(manager, custom),
            cmd_errors:  r.commands.errors.unwrap_or_else(|| concat!(
                "journalctl -b -p emerg..alert -q --no-pager -o short 2>/dev/null",
                " | grep -Ev 'kactivitymanagerd|pam_unix|kglobalaccel|applications\\.menu'",
                " | grep -cE '^[^ ]'",
            ).into()),
        }
    }

    fn load() -> Self {
        let raw = config_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| match toml::from_str::<RawConfig>(&s) {
                Ok(r)  => Some(r),
                Err(e) => { eprintln!("greeting: config error: {e}"); None }
            })
            .unwrap_or_default();
        Config::resolve(raw)
    }
}

fn updates_cmd(manager: &str, custom: &str) -> String {
    match manager {
        "pacman"  => "checkupdates 2>/dev/null | wc -l".into(),
        "apt"     => "apt list --upgradable 2>/dev/null | grep -c upgradable".into(),
        "dnf"     => "dnf check-update -q 2>/dev/null | grep -cv '^$'".into(),
        "brew"    => "brew outdated 2>/dev/null | wc -l | tr -d ' '".into(),
        "zypper"  => "zypper lu 2>/dev/null | grep -c '^v '".into(),
        "flatpak" => "flatpak remote-ls --updates 2>/dev/null | wc -l".into(),
        "nix"     => "nix-env -u --dry-run 2>&1 | grep -c 'installing'".into(),
        _         => custom.to_string(),
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var(var).ok().filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(fallback)
        })
}

fn config_path() -> Option<PathBuf> {
    let p = xdg("XDG_CONFIG_HOME", ".config").join("greeting").join("greeting.toml");
    p.exists().then_some(p)
}

fn config_init_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("greeting").join("greeting.toml")
}

fn cache_dir(c: &Config) -> PathBuf {
    c.cache_dir.as_ref().map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_CACHE_HOME", ".cache").join("greeting"))
}

fn data_path(c: &Config)  -> PathBuf { cache_dir(c).join("data.cache")  }
fn shown_path(c: &Config) -> PathBuf { cache_dir(c).join("shown.cache") }

// ── Time ──────────────────────────────────────────────────────────────────────

#[repr(C)]
struct Tm { sec:i32, min:i32, hour:i32, mday:i32, mon:i32, year:i32, wday:i32, yday:i32, isdst:i32 }

extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime(t: *const i64) -> *const Tm;
    fn getuid() -> u32;
}

fn now_tm() -> (i64, Tm) {
    unsafe {
        let mut t = 0i64;
        time(&mut t);
        (t, std::ptr::read(localtime(&t)))
    }
}

fn strftime(fmt: &str, tm: &Tm) -> String {
    let mon = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let day = ["Sun","Mon","Tue","Wed","Thu","Fri","Sat"];
    let mut out = String::with_capacity(32);
    let mut it  = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' { out.push(c); continue; }
        match it.next() {
            Some('Y') => out.push_str(&format!("{:04}", tm.year + 1900)),
            Some('y') => out.push_str(&format!("{:02}", (tm.year + 1900) % 100)),
            Some('m') => out.push_str(&format!("{:02}", tm.mon + 1)),
            Some('d') => out.push_str(&format!("{:02}", tm.mday)),
            Some('H') => out.push_str(&format!("{:02}", tm.hour)),
            Some('M') => out.push_str(&format!("{:02}", tm.min)),
            Some('S') => out.push_str(&format!("{:02}", tm.sec)),
            Some('b') | Some('B') => out.push_str(mon[tm.mon.clamp(0, 11) as usize]),
            Some('a') | Some('A') => out.push_str(day[tm.wday.clamp(0, 6) as usize]),
            Some(o)  => { out.push('%'); out.push(o); }
            None     => out.push('%'),
        }
    }
    out
}

fn time_of_day(hour: i32, lang: &str) -> &'static str {
    match lang {
        "ru" | "be" => match hour { 5..=11=>"утро",      12..=16=>"день",         17..=20=>"вечер",   _=>"ночь"    },
        "uk"        => match hour { 5..=11=>"ранок",     12..=16=>"день",         17..=20=>"вечір",   _=>"ніч"     },
        "pl"        => match hour { 5..=11=>"ranek",     12..=16=>"południe",     17..=20=>"wieczór", _=>"noc"     },
        "cs"        => match hour { 5..=11=>"ráno",      12..=16=>"odpoledne",    17..=20=>"večer",   _=>"noc"     },
        "sk"        => match hour { 5..=11=>"ráno",      12..=16=>"poobede",      17..=20=>"večer",   _=>"noc"     },
        "hr" | "sr" => match hour { 5..=11=>"jutro",     12..=16=>"podne",        17..=20=>"večer",   _=>"noć"     },
        "sl"        => match hour { 5..=11=>"jutro",     12..=16=>"popoldan",     17..=20=>"večer",   _=>"noč"     },
        "bg"        => match hour { 5..=11=>"утро",      12..=16=>"следобед",     17..=20=>"вечер",   _=>"нощ"     },
        "es"        => match hour { 5..=11=>"mañana",    12..=16=>"tarde",        17..=20=>"tarde",   _=>"noche"   },
        "pt"        => match hour { 5..=11=>"manhã",     12..=16=>"tarde",        17..=20=>"tarde",   _=>"noite"   },
        "fr"        => match hour { 5..=11=>"matin",     12..=16=>"après-midi",   17..=20=>"soir",    _=>"nuit"    },
        "it"        => match hour { 5..=11=>"mattina",   12..=16=>"pomeriggio",   17..=20=>"sera",    _=>"notte"   },
        "ro"        => match hour { 5..=11=>"dimineață", 12..=16=>"amiază",       17..=20=>"seară",   _=>"noapte"  },
        "ca"        => match hour { 5..=11=>"matí",      12..=16=>"tarda",        17..=20=>"tarda",   _=>"nit"     },
        "de"        => match hour { 5..=11=>"Morgen",    12..=16=>"Nachmittag",   17..=20=>"Abend",   _=>"Nacht"   },
        "nl"        => match hour { 5..=11=>"morgen",    12..=16=>"middag",       17..=20=>"avond",   _=>"nacht"   },
        "sv"        => match hour { 5..=11=>"morgon",    12..=16=>"eftermiddag",  17..=20=>"kväll",   _=>"natt"    },
        "nb" | "nn" => match hour { 5..=11=>"morgen",    12..=16=>"ettermiddag",  17..=20=>"kveld",   _=>"natt"    },
        "da"        => match hour { 5..=11=>"morgen",    12..=16=>"eftermiddag",  17..=20=>"aften",   _=>"nat"     },
        "fi"        => match hour { 5..=11=>"aamu",      12..=16=>"iltapäivä",    17..=20=>"ilta",    _=>"yö"      },
        "is"        => match hour { 5..=11=>"morgunn",   12..=16=>"eftirmiddag",  17..=20=>"kvöld",   _=>"nótt"    },
        "lt"        => match hour { 5..=11=>"rytas",     12..=16=>"popietė",      17..=20=>"vakaras", _=>"naktis"  },
        "lv"        => match hour { 5..=11=>"rīts",      12..=16=>"pēcpusdiena",  17..=20=>"vakars",  _=>"nakts"   },
        "el"        => match hour { 5..=11=>"πρωί",      12..=16=>"απόγευμα",     17..=20=>"βράδυ",   _=>"νύχτα"   },
        "hu"        => match hour { 5..=11=>"reggel",    12..=16=>"délután",      17..=20=>"este",    _=>"éjszaka" },
        "tr"        => match hour { 5..=11=>"sabah",     12..=16=>"öğleden sonra",17..=20=>"akşam",   _=>"gece"    },
        _           => match hour { 5..=11=>"morning",   12..=16=>"afternoon",    17..=20=>"evening", _=>"night"   },
    }
}

// ── Cache ─────────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs()
}

struct DataCache { written_at: u64, updates: i32, errors: i32 }

fn read_data(c: &Config) -> Option<DataCache> {
    let s = fs::read_to_string(data_path(c)).ok()?;
    let mut p = s.split_whitespace();
    Some(DataCache {
        written_at: p.next()?.parse().ok()?,
        updates:    p.next()?.parse().ok()?,
        errors:     p.next()?.parse().ok()?,
    })
}

fn write_data(c: &Config, updates: i32, errors: i32) {
    fs::create_dir_all(cache_dir(c)).ok();
    let _ = fs::write(data_path(c), format!("{} {} {}\n", unix_now(), updates, errors));
}

fn shown_age(c: &Config) -> Option<u64> {
    let ts: u64 = fs::read_to_string(shown_path(c)).ok()?.trim().parse().ok()?;
    Some(unix_now().saturating_sub(ts))
}

fn touch_shown(c: &Config) {
    fs::create_dir_all(cache_dir(c)).ok();
    let _ = fs::write(shown_path(c), unix_now().to_string());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn expand(tpl: &str, vars: &[(&str, &str)]) -> String {
    let mut s = tpl.to_string();
    for (k, v) in vars { s = s.replace(&format!("{{{k}}}"), v); }
    s
}

fn username() -> String {
    if let Ok(u) = std::env::var("USER") { if !u.is_empty() { return u; } }
    let uid = unsafe { getuid() };
    if let Ok(p) = fs::read_to_string("/etc/passwd") {
        for line in p.lines() {
            let mut f = line.split(':');
            let name = f.next().unwrap_or("");
            let _    = f.next();
            let luid: u32 = f.next().unwrap_or("").parse().unwrap_or(u32::MAX);
            if luid == uid { return name.to_string(); }
        }
    }
    "stranger".to_string()
}

fn run_count(cmd: &str) -> Option<i32> {
    let mut child = Command::new("sh").args(["-c", cmd])
        .stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn collect(c: &Config) -> Option<(i32, i32)> {
    let c1 = c.cmd_updates.clone();
    let c2 = c.cmd_errors.clone();
    let h1 = std::thread::spawn(move || run_count(&c1));
    let h2 = std::thread::spawn(move || run_count(&c2));
    Some((h1.join().ok()??, h2.join().ok()??))
}

// ── Subcommands ───────────────────────────────────────────────────────────────

fn cmd_show(c: &Config, force: bool) {
    if !force && c.show_cooldown > 0 {
        if let Some(age) = shown_age(c) {
            if age < c.show_cooldown { return; }
        }
    }
    touch_shown(c);

    let (ts, tm) = now_tm();
    let date = strftime(&c.date_format, &tm);
    let time = strftime(&c.time_format, &tm);
    let tod  = time_of_day(tm.hour, &c.lang);
    let user = username();

    let mut updates = -1;
    let mut errors = -1;
    let mut stale = match read_data(c) {
        Some(d) => {
            updates = d.updates;
            errors = d.errors;
            (ts as u64).saturating_sub(d.written_at) > c.update_interval + 300
        }
        None => true,
    };
    if stale {
        if let Some((u, e)) = collect(c) {
            write_data(c, u, e);
            updates = u;
            errors = e;
            stale = false;
        }
    }

    let upd_s = updates.to_string();
    let err_s = errors.to_string();

    let vars: &[(&str, &str)] = &[
        ("user",        user.as_str()),
        ("time_of_day", tod),
        ("date",        date.as_str()),
        ("time",        time.as_str()),
        ("updates",     upd_s.as_str()),
        ("errors",      err_s.as_str()),
        ("c_green",  &c.c_green),  ("c_yellow", &c.c_yellow),
        ("c_purple", &c.c_purple), ("c_red",    &c.c_red),
        ("c_reset",  &c.c_reset),  ("c_bold",   &c.c_bold),
        ("c_dim",    &c.c_dim),
    ];

    println!("{}", expand(&c.tpl_greeting, vars));

    if let Some(ref tpl) = c.tpl_status_line {
        println!("{}", expand(tpl, vars));
    } else {
        let upd = if updates < 0 {
            format!("{}updates: checking…{}", c.c_yellow, c.c_reset)
        } else if updates == 0 {
            expand(&c.tpl_updates_none, vars)
        } else {
            expand(&c.tpl_updates_some, vars)
        };
        let err = if errors < 0 {
            format!("{}errors: checking…{}", c.c_yellow, c.c_reset)
        } else if errors == 0 {
            expand(&c.tpl_errors_none, vars)
        } else {
            expand(&c.tpl_errors_some, vars)
        };
        println!("{upd}  {err}");
    }

    if stale {
        eprintln!("{}⚠  data cache is stale — is 'greeting daemon' running?{}", c.c_yellow, c.c_reset);
    }
}

fn cmd_update(c: &Config) {
    eprintln!("collecting…");
    match collect(c) {
        Some((u, e)) => {
            write_data(c, u, e);
            eprintln!("{u} updates, {e} errors → {}", data_path(c).display());
        }
        None => eprintln!("collection failed — cache not updated"),
    }
}

fn cmd_status(c: &Config) {
    let cfg = config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("{} (not found, using defaults)", config_init_path().display()));
    println!("config:    {cfg}");
    println!("cache dir: {}", cache_dir(c).display());
    println!("cooldown:  {}s ({} min)", c.show_cooldown, c.show_cooldown / 60);
    println!("interval:  {}s ({} h)",  c.update_interval, c.update_interval / 3600);
    match shown_age(c) {
        Some(a) => println!("shown:     {}s ago", a),
        None    => println!("shown:     never"),
    }
    match read_data(c) {
        Some(d) => println!("cache:     {}s old — {} updates, {} errors",
                            unix_now().saturating_sub(d.written_at), d.updates, d.errors),
        None    => println!("cache:     empty  (run: greeting update)"),
    }
}

extern "C" {
    fn openlog(ident: *const i8, option: i32, facility: i32);
    fn syslog(priority: i32, msg: *const i8, ...);
}

fn slog(msg: &str) {
    if let Ok(s) = std::ffi::CString::new(msg) {
        unsafe { syslog(6, s.as_ptr()); }
    }
}

fn cmd_daemon(c: &Config) {
    let tag = std::ffi::CString::new("greeting").unwrap();
    unsafe { openlog(tag.as_ptr(), 0, 8); }
    slog("started");
    loop {
        let ok = match collect(c) {
            Some((u, e)) => {
                write_data(c, u, e);
                slog(&format!("updated: {u} updates, {e} errors"));
                true
            }
            None => {
                slog("collect failed, retrying");
                false
            }
        };
        let delay = if ok { c.update_interval } else { 300 };
        let next = unix_now() + delay;
        while unix_now() < next {
            std::thread::sleep(Duration::from_secs((next - unix_now()).min(30)));
        }
    }
}

fn cmd_init() {
    let path = config_init_path();
    if let Some(p) = path.parent() { fs::create_dir_all(p).ok(); }
    if path.exists() {
        eprintln!("greeting: config already exists: {}", path.display());
        eprintln!("          delete it first if you want to reset to defaults.");
        return;
    }
    let content = r#"# greeting.toml — all fields are optional, remove any line to use the default

[cache]
show_cooldown   = 14400  # seconds between greeting displays (0 = always show)
update_interval = 28800  # seconds between daemon refreshes
# dir = "/home/user/.cache/greeting"

[display]
date_format = "%b %d, %a"
time_format = "%H:%M"
# Language for time-of-day words.
# Supported: en de fr es pt it nl pl ru uk cs sk hr sr sl bg ro
#            sv nb nn da fi is lt lv el hu tr be ca
lang = "en"

[colors]
enabled = true   # set to false to disable all ANSI colors

[templates]
# Available variables: {user} {time_of_day} {date} {time} {updates} {errors}
# Color tags:          {c_green} {c_yellow} {c_purple} {c_red} {c_reset} {c_bold} {c_dim}

greeting     = "Welcome, {c_green}{user}{c_reset}. Good {c_purple}{time_of_day}{c_reset}. {c_yellow}{date}{c_reset} {c_green}{time}{c_reset}"
updates_none = "Fresh as ever — {c_purple}no updates{c_reset}"
updates_some = "A fresh batch — {c_green}{updates} updates{c_reset}"
errors_none  = "Errors: {c_green}{errors}{c_reset}"
errors_some  = "Errors: {c_red}{errors}{c_reset}"

# Uncomment to replace updates + errors lines with a single custom line:
# status_line = "{c_dim}upd:{c_reset} {updates}  {c_dim}err:{c_reset} {errors}"

[updates]
# Package manager: pacman | apt | dnf | brew | zypper | flatpak | nix | custom
manager = "pacman"
# command = "my-cmd | wc -l"   # used when manager = "custom"
"#;
    match fs::write(&path, content) {
        Ok(_)  => println!("created: {}", path.display()),
        Err(e) => eprintln!("error: {e}"),
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");

    if sub == "init" { cmd_init(); return; }

    let cfg = Config::load();

    match sub {
        "" | "show" => cmd_show(&cfg, sub == "show"),
        "daemon"    => cmd_daemon(&cfg),
        "update"    => cmd_update(&cfg),
        "status"    => cmd_status(&cfg),
        "help" | "--help" | "-h" => print!(concat!(
            "usage: greeting [subcommand]\n\n",
            "  (none)   show greeting, respects cooldown\n",
            "  show     show greeting unconditionally\n",
            "  daemon   background daemon — refreshes data cache\n",
            "  update   collect data once and exit\n",
            "  status   show cache and config state\n",
            "  init     create default config (~/.config/greeting/greeting.toml)\n",
        )),
        other => {
            eprintln!("greeting: unknown subcommand '{other}' — try 'greeting help'");
            std::process::exit(1);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> Config { Config::resolve(RawConfig::default()) }

    // — package manager presets —

    #[test]
    fn pm_pacman()  { assert!(updates_cmd("pacman",  "").contains("checkupdates")); }
    #[test]
    fn pm_apt()     { assert!(updates_cmd("apt",     "").contains("apt list")); }
    #[test]
    fn pm_dnf()     { assert!(updates_cmd("dnf",     "").contains("dnf check-update")); }
    #[test]
    fn pm_brew()    { assert!(updates_cmd("brew",    "").contains("brew outdated")); }
    #[test]
    fn pm_zypper()  { assert!(updates_cmd("zypper",  "").contains("zypper")); }
    #[test]
    fn pm_flatpak() { assert!(updates_cmd("flatpak", "").contains("flatpak")); }
    #[test]
    fn pm_nix()     { assert!(updates_cmd("nix",     "").contains("nix")); }
    #[test]
    fn pm_custom()  { assert_eq!(updates_cmd("custom", "my-cmd | wc -l"), "my-cmd | wc -l"); }
    #[test]
    fn pm_unknown_uses_custom() { assert_eq!(updates_cmd("xbps", "xbps | wc -l"), "xbps | wc -l"); }

    // — config defaults —

    #[test]
    fn defaults_sane() {
        let c = default_cfg();
        assert_eq!(c.show_cooldown,   14400);
        assert_eq!(c.update_interval, 28800);
        assert_eq!(c.lang,            "en");
        assert!(c.tpl_status_line.is_none());
        assert!(!c.c_green.is_empty());
    }

    #[test]
    fn colors_disabled() {
        let mut r = RawConfig::default();
        r.colors.enabled = Some(false);
        let c = Config::resolve(r);
        assert!(c.c_green.is_empty());
        assert!(c.c_red.is_empty());
        assert!(c.c_reset.is_empty());
    }

    // — TOML parsing —

    #[test]
    fn toml_basic() {
        let r: RawConfig = toml::from_str(r#"
            [cache]
            show_cooldown = 3600
            [display]
            lang = "de"
            [updates]
            manager = "apt"
        "#).unwrap();
        let c = Config::resolve(r);
        assert_eq!(c.show_cooldown, 3600);
        assert_eq!(c.lang, "de");
        assert!(c.cmd_updates.contains("apt list"));
    }

    #[test]
    fn toml_unknown_fields_error() {
        // deny_unknown_fields catches typos in config
        let r = toml::from_str::<RawConfig>("[cache]\ntypo_field = 1");
        assert!(r.is_err());
    }

    #[test]
    fn toml_empty_is_all_defaults() {
        let r: RawConfig = toml::from_str("").unwrap();
        let c = Config::resolve(r);
        assert_eq!(c.show_cooldown, 14400);
        assert_eq!(c.lang, "en");
    }

    // — template expansion —

    #[test]
    fn expand_basic() {
        let r = expand("Hello {name}!", &[("name", "world")]);
        assert_eq!(r, "Hello world!");
    }

    #[test]
    fn expand_unknown_var_preserved() {
        let r = expand("{unknown}", &[]);
        assert_eq!(r, "{unknown}");
    }

    #[test]
    fn expand_multiple() {
        let r = expand("{a}{b}{a}", &[("a", "1"), ("b", "2")]);
        assert_eq!(r, "121");
    }

    // — time_of_day —

    #[test]
    fn tod_boundaries() {
        assert_eq!(time_of_day(4,  "en"), "night");
        assert_eq!(time_of_day(5,  "en"), "morning");
        assert_eq!(time_of_day(11, "en"), "morning");
        assert_eq!(time_of_day(12, "en"), "afternoon");
        assert_eq!(time_of_day(17, "en"), "evening");
        assert_eq!(time_of_day(20, "en"), "evening");
        assert_eq!(time_of_day(21, "en"), "night");
    }

    #[test]
    fn tod_languages() {
        assert_eq!(time_of_day(8, "de"), "Morgen");
        assert_eq!(time_of_day(8, "ru"), "утро");
        assert_eq!(time_of_day(8, "fr"), "matin");
        assert_eq!(time_of_day(8, "uk"), "ранок");
        assert_eq!(time_of_day(8, "fi"), "aamu");
    }

    #[test]
    fn tod_unknown_lang_is_english() {
        assert_eq!(time_of_day(9, "zz"), "morning");
    }
}
