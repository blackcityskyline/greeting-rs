//! greeting — terminal greeter with daemon mode
//!
//! greeting           — show (respects cooldown)
//! greeting show      — show (ignore cooldown)
//! greeting daemon    — background daemon, updates cache on interval
//! greeting update    — collect data once, write cache, exit
//! greeting status    — show cache/config state
//! greeting init      — write default config

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheConfig {
    show_cooldown:   Option<u64>,
    update_interval: Option<u64>,
    cache_dir:       Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DisplayConfig {
    date_format: Option<String>,
    time_format: Option<String>,
    lang:        Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ColorsConfig {
    all:    Option<String>,
    green:  Option<String>,
    yellow: Option<String>,
    purple: Option<String>,
    red:    Option<String>,
    reset:  Option<String>,
    bold:   Option<String>,
    dim:    Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TemplatesConfig {
    greeting:     Option<String>,
    status_line:  Option<String>,
    updates_none: Option<String>,
    updates_some: Option<String>,
    errors_none:  Option<String>,
    errors_some:  Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdatesConfig {
    manager: Option<String>,
    custom:  Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CommandsConfig {
    errors: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    cache:     CacheConfig,
    display:   DisplayConfig,
    colors:    ColorsConfig,
    templates: TemplatesConfig,
    updates:   UpdatesConfig,
    commands:  CommandsConfig,
}

// Resolved config with all defaults filled in
struct Config {
    show_cooldown:   u64,
    update_interval: u64,
    cache_dir:       Option<String>,
    date_format:     String,
    time_format:     String,
    lang:            String,
    c_green:  String, c_yellow: String, c_purple: String,
    c_red:    String, c_reset:  String, c_bold:   String, c_dim: String,
    tpl_greeting:     String,
    tpl_status_line:  Option<String>,
    tpl_updates_none: String,
    tpl_updates_some: String,
    tpl_errors_none:  String,
    tpl_errors_some:  String,
    cmd_updates:      String,
    cmd_errors:       String,
}

fn color_or(opt: Option<String>, disabled: bool, default: &str) -> String {
    if disabled { return String::new(); }
    match opt {
        Some(v) if v == "none" => String::new(),
        Some(v) => v,
        None    => default.into(),
    }
}

impl Config {
    fn from_raw(r: RawConfig) -> Self {
        let disable_all = r.colors.all.as_deref() == Some("none");
        let pkg = r.updates.manager.as_deref().unwrap_or("pacman");
        let custom = r.updates.custom.as_deref().unwrap_or("");
        Config {
            show_cooldown:   r.cache.show_cooldown.unwrap_or(14400),
            update_interval: r.cache.update_interval.unwrap_or(28800),
            cache_dir:       r.cache.cache_dir,
            date_format:     r.display.date_format.unwrap_or_else(|| "%b %d, %a".into()),
            time_format:     r.display.time_format.unwrap_or_else(|| "%H:%M".into()),
            lang:            r.display.lang.unwrap_or_else(|| "en".into()),
            c_green:  color_or(r.colors.green,  disable_all, "\x1b[32m"),
            c_yellow: color_or(r.colors.yellow, disable_all, "\x1b[33m"),
            c_purple: color_or(r.colors.purple, disable_all, "\x1b[38;5;147m"),
            c_red:    color_or(r.colors.red,    disable_all, "\x1b[31m"),
            c_reset:  color_or(r.colors.reset,  disable_all, "\x1b[0m"),
            c_bold:   color_or(r.colors.bold,   disable_all, "\x1b[1m"),
            c_dim:    color_or(r.colors.dim,    disable_all, "\x1b[2m"),
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
                "A fresh batch — {c_green}{updates} updates available{c_reset}".into()),
            tpl_errors_none: r.templates.errors_none.unwrap_or_else(||
                "Critical errors: {c_green}{errors}{c_reset}".into()),
            tpl_errors_some: r.templates.errors_some.unwrap_or_else(||
                "Critical errors: {c_red}{errors}{c_reset}".into()),
            cmd_updates: pkg_manager_cmd(pkg, custom),
            cmd_errors: r.commands.errors.unwrap_or_else(|| concat!(
                "journalctl -b -p emerg..alert -q --no-pager -o short 2>/dev/null",
                " | grep -Ev 'kactivitymanagerd|pam_unix|kglobalaccel|applications\\.menu'",
                " | grep -cE '^[^ ]'",
            ).into()),
        }
    }

    fn load() -> Self {
        let raw: RawConfig = config_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).map_err(|e| {
                eprintln!("greeting: config parse error: {e}");
            }).ok())
            .unwrap_or_default();
        Config::from_raw(raw)
    }
}

fn pkg_manager_cmd(manager: &str, custom: &str) -> String {
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
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(fallback)
        })
}

fn config_path() -> Option<PathBuf> {
    let p = xdg("XDG_CONFIG_HOME", ".config").join("greeting").join("greeting.toml");
    if p.exists() { Some(p) } else { None }
}

fn config_path_for_init() -> PathBuf {
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
struct Tm {
    sec:i32, min:i32, hour:i32, mday:i32,
    mon:i32, year:i32, wday:i32, yday:i32, isdst:i32,
}

extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime(t: *const i64) -> *const Tm;
    fn getuid() -> u32;
}

fn now_tm() -> (i64, Tm) {
    unsafe {
        let mut t: i64 = 0;
        time(&mut t);
        let ptr = localtime(&t);
        (t, std::ptr::read(ptr))
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
            Some('Y') => out.push_str(&format!("{:04}", tm.year+1900)),
            Some('y') => out.push_str(&format!("{:02}", (tm.year+1900)%100)),
            Some('m') => out.push_str(&format!("{:02}", tm.mon+1)),
            Some('d') => out.push_str(&format!("{:02}", tm.mday)),
            Some('H') => out.push_str(&format!("{:02}", tm.hour)),
            Some('M') => out.push_str(&format!("{:02}", tm.min)),
            Some('S') => out.push_str(&format!("{:02}", tm.sec)),
            Some('b') | Some('B') => out.push_str(mon[tm.mon.clamp(0,11) as usize]),
            Some('a') | Some('A') => out.push_str(day[tm.wday.clamp(0,6) as usize]),
            Some(o)  => { out.push('%'); out.push(o); }
            None     => out.push('%'),
        }
    }
    out
}

fn time_of_day(hour: i32, lang: &str) -> &'static str {
    match lang {
        "ru" | "be" => match hour { 5..=11=>"утро",     12..=16=>"день",        17..=20=>"вечер",   _=>"ночь"     },
        "pl"        => match hour { 5..=11=>"ranek",    12..=16=>"południe",    17..=20=>"wieczór", _=>"noc"      },
        "cs"        => match hour { 5..=11=>"ráno",     12..=16=>"odpoledne",   17..=20=>"večer",   _=>"noc"      },
        "sk"        => match hour { 5..=11=>"ráno",     12..=16=>"poobede",     17..=20=>"večer",   _=>"noc"      },
        "hr" | "sr" => match hour { 5..=11=>"jutro",    12..=16=>"podne",       17..=20=>"večer",   _=>"noć"      },
        "sl"        => match hour { 5..=11=>"jutro",    12..=16=>"popoldan",    17..=20=>"večer",   _=>"noč"      },
        "bg"        => match hour { 5..=11=>"утро",     12..=16=>"следобед",    17..=20=>"вечер",   _=>"нощ"      },
        "es"        => match hour { 5..=11=>"mañana",   12..=16=>"tarde",       17..=20=>"tarde",   _=>"noche"    },
        "pt"        => match hour { 5..=11=>"manhã",    12..=16=>"tarde",       17..=20=>"tarde",   _=>"noite"    },
        "fr"        => match hour { 5..=11=>"matin",    12..=16=>"après-midi",  17..=20=>"soir",    _=>"nuit"     },
        "it"        => match hour { 5..=11=>"mattina",  12..=16=>"pomeriggio",  17..=20=>"sera",    _=>"notte"    },
        "ro"        => match hour { 5..=11=>"dimineață",12..=16=>"amiază",      17..=20=>"seară",   _=>"noapte"   },
        "ca"        => match hour { 5..=11=>"matí",     12..=16=>"tarda",       17..=20=>"tarda",   _=>"nit"      },
        "de"        => match hour { 5..=11=>"Morgen",   12..=16=>"Nachmittag",  17..=20=>"Abend",   _=>"Nacht"    },
        "nl"        => match hour { 5..=11=>"morgen",   12..=16=>"middag",      17..=20=>"avond",   _=>"nacht"    },
        "sv"        => match hour { 5..=11=>"morgon",   12..=16=>"eftermiddag", 17..=20=>"kväll",   _=>"natt"     },
        "nb" | "nn" => match hour { 5..=11=>"morgen",   12..=16=>"ettermiddag", 17..=20=>"kveld",   _=>"natt"     },
        "da"        => match hour { 5..=11=>"morgen",   12..=16=>"eftermiddag", 17..=20=>"aften",   _=>"nat"      },
        "fi"        => match hour { 5..=11=>"aamu",     12..=16=>"iltapäivä",   17..=20=>"ilta",    _=>"yö"       },
        "is"        => match hour { 5..=11=>"morgunn",  12..=16=>"eftirmiddag", 17..=20=>"kvöld",   _=>"nótt"     },
        "el"        => match hour { 5..=11=>"πρωί",     12..=16=>"απόγευμα",    17..=20=>"βράδυ",   _=>"νύχτα"    },
        "hu"        => match hour { 5..=11=>"reggel",   12..=16=>"délután",     17..=20=>"este",    _=>"éjszaka"  },
        "tr"        => match hour { 5..=11=>"sabah",    12..=16=>"öğleden sonra",17..=20=>"akşam",  _=>"gece"     },
        _           => match hour { 5..=11=>"morning",  12..=16=>"afternoon",   17..=20=>"evening", _=>"night"    },
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

// ── Template ──────────────────────────────────────────────────────────────────

fn expand(tpl: &str, vars: &[(&str, &str)]) -> String {
    let mut s = tpl.to_string();
    for (k, v) in vars { s = s.replace(&format!("{{{k}}}"), v); }
    s
}

// ── Username ──────────────────────────────────────────────────────────────────

fn username() -> String {
    if let Ok(u) = std::env::var("USER") { if !u.is_empty() { return u; } }
    let uid = unsafe { getuid() };
    if let Ok(p) = fs::read_to_string("/etc/passwd") {
        for line in p.lines() {
            let mut f = line.split(':');
            let name = f.next().unwrap_or("");
            let _ = f.next();
            let luid: u32 = f.next().unwrap_or("").parse().unwrap_or(u32::MAX);
            if luid == uid { return name.to_string(); }
        }
    }
    "stranger".to_string()
}

// ── Collect ───────────────────────────────────────────────────────────────────

fn run_count(cmd: &str) -> i32 {
    Command::new("sh").args(["-c", cmd])
        .stdout(Stdio::piped()).stderr(Stdio::null())
        .output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn collect(c: &Config) -> (i32, i32) {
    let c1 = c.cmd_updates.clone();
    let c2 = c.cmd_errors.clone();
    let h1 = std::thread::spawn(move || run_count(&c1));
    let h2 = std::thread::spawn(move || run_count(&c2));
    (h1.join().unwrap_or(0), h2.join().unwrap_or(0))
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

    let (updates, errors, stale) = match read_data(c) {
        Some(d) => {
            let age = (ts as u64).saturating_sub(d.written_at);
            (d.updates, d.errors, age > c.update_interval + 300)
        }
        None => (-1, -1, false),
    };

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
            format!("{}Updates: checking…{}", c.c_yellow, c.c_reset)
        } else if updates == 0 {
            expand(&c.tpl_updates_none, vars)
        } else {
            expand(&c.tpl_updates_some, vars)
        };
        let err = if errors < 0 {
            format!("{}Errors: checking…{}", c.c_yellow, c.c_reset)
        } else if errors == 0 {
            expand(&c.tpl_errors_none, vars)
        } else {
            expand(&c.tpl_errors_some, vars)
        };
        println!("{upd}  {err}");
    }

    if stale {
        eprintln!("{}⚠ Data cache is stale — is 'greeting daemon' running?{}", c.c_yellow, c.c_reset);
    }
}

fn cmd_update(c: &Config) {
    eprintln!("Collecting system data…");
    let (u, e) = collect(c);
    write_data(c, u, e);
    eprintln!("Done: {u} updates, {e} errors → {}", data_path(c).display());
}

fn cmd_status(c: &Config) {
    let cfg_display = config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("{} (not found, using defaults)",
            config_path_for_init().display()));
    println!("Config:     {cfg_display}");
    println!("Cache dir:  {}", cache_dir(c).display());
    println!("Cooldown:   {}s  ({} min)", c.show_cooldown, c.show_cooldown / 60);
    println!("Interval:   {}s  ({} h)",   c.update_interval, c.update_interval / 3600);
    match shown_age(c) {
        Some(a) => println!("Last shown: {}s ago", a),
        None    => println!("Last shown: never"),
    }
    match read_data(c) {
        Some(d) => println!("Data cache: {}s old  —  {} updates, {} errors",
                             unix_now().saturating_sub(d.written_at), d.updates, d.errors),
        None    => println!("Data cache: empty (run 'greeting update')"),
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
    slog("daemon started");
    loop {
        let (u, e) = collect(c);
        write_data(c, u, e);
        slog(&format!("cache updated: {u} updates, {e} errors"));
        std::thread::sleep(Duration::from_secs(c.update_interval));
    }
}

fn cmd_init() {
    let path = config_path_for_init();
    if let Some(p) = path.parent() { fs::create_dir_all(p).ok(); }
    let content = r#"# greeting.toml
# All fields optional — defaults are used for anything omitted.

[cache]
show_cooldown   = 14400   # seconds between greeting displays (0 = always)
update_interval = 28800   # seconds between daemon data refreshes
# cache_dir = "/home/user/.cache/greeting"

[display]
date_format = "%b %d, %a"
time_format = "%H:%M"
# Supported langs: en de fr es pt it nl pl ru cs sk hr sr sl bg ro
#                  sv nb nn da fi is el hu tr be ca
lang = "en"

[colors]
# Set any to "none" to disable. Use  all = "none"  to strip all ANSI codes.
# all    = "none"
green  = "\u001b[32m"
yellow = "\u001b[33m"
purple = "\u001b[38;5;147m"
red    = "\u001b[31m"
reset  = "\u001b[0m"
bold   = "\u001b[1m"
dim    = "\u001b[2m"

[templates]
# Variables: {user} {time_of_day} {date} {time} {updates} {errors}
# Colors:    {c_green} {c_yellow} {c_purple} {c_red} {c_reset} {c_bold} {c_dim}

greeting     = "Welcome, {c_green}{user}{c_reset}. Good {c_purple}{time_of_day}{c_reset}. Date: {c_yellow}{date}{c_reset} / Time: {c_green}{time}{c_reset}"

# status_line replaces updates_*/errors_* with a single custom second line:
# status_line  = "{c_dim}upd:{c_reset} {updates}  {c_dim}err:{c_reset} {errors}"

updates_none = "Fresh as ever — {c_purple}no updates{c_reset}"
updates_some = "A fresh batch — {c_green}{updates} updates available{c_reset}"
errors_none  = "Critical errors: {c_green}{errors}{c_reset}"
errors_some  = "Critical errors: {c_red}{errors}{c_reset}"

[updates]
# Supported: pacman apt dnf brew zypper flatpak nix custom
manager = "pacman"
# custom = "my-check-cmd | wc -l"

[commands]
# Override the errors command — must print a single integer to stdout.
errors = "journalctl -b -p emerg..alert -q --no-pager -o short 2>/dev/null | grep -Ev 'kactivitymanagerd|pam_unix|kglobalaccel|applications\\.menu' | grep -cE '^[^ ]'"
"#;
    match fs::write(&path, content) {
        Ok(_)  => println!("Config written: {}", path.display()),
        Err(e) => eprintln!("Cannot write config: {e}"),
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
        "help" | "--help" | "-h" => println!(
            "Usage: greeting [subcommand]\n\n  \
             (none)   Show greeting (respects show_cooldown)\n  \
             show     Show greeting unconditionally\n  \
             daemon   Run background daemon (refreshes data cache)\n  \
             update   Collect data once and write cache, then exit\n  \
             status   Show cache and config state\n  \
             init     Write default config to XDG_CONFIG_HOME/greeting/greeting.toml"
        ),
        other => {
            eprintln!("greeting: unknown subcommand '{other}'. Try 'greeting help'.");
            std::process::exit(1);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_manager_presets() {
        assert!(pkg_manager_cmd("pacman", "").contains("checkupdates"));
        assert!(pkg_manager_cmd("apt",    "").contains("apt list"));
        assert!(pkg_manager_cmd("dnf",    "").contains("dnf check-update"));
        assert!(pkg_manager_cmd("brew",   "").contains("brew outdated"));
        assert!(pkg_manager_cmd("zypper", "").contains("zypper"));
        assert!(pkg_manager_cmd("flatpak","").contains("flatpak"));
        assert!(pkg_manager_cmd("nix",    "").contains("nix"));
    }

    #[test]
    fn pkg_manager_custom() {
        let cmd = pkg_manager_cmd("custom", "my-tool | wc -l");
        assert_eq!(cmd, "my-tool | wc -l");
        // unknown manager also falls through to custom
        let cmd2 = pkg_manager_cmd("xbps", "xbps-install -un | wc -l");
        assert_eq!(cmd2, "xbps-install -un | wc -l");
    }

    #[test]
    fn config_defaults_without_file() {
        let cfg = Config::from_raw(RawConfig::default());
        assert_eq!(cfg.show_cooldown,   14400);
        assert_eq!(cfg.update_interval, 28800);
        assert_eq!(cfg.lang,            "en");
        assert_eq!(cfg.date_format,     "%b %d, %a");
        assert!(cfg.c_green.contains("32"));
        assert!(cfg.tpl_status_line.is_none());
    }

    #[test]
    fn config_colors_all_none() {
        let mut raw = RawConfig::default();
        raw.colors.all = Some("none".into());
        let cfg = Config::from_raw(raw);
        assert!(cfg.c_green.is_empty());
        assert!(cfg.c_red.is_empty());
        assert!(cfg.c_reset.is_empty());
    }

    #[test]
    fn config_color_individual_none() {
        let mut raw = RawConfig::default();
        raw.colors.red = Some("none".into());
        let cfg = Config::from_raw(raw);
        assert!(cfg.c_red.is_empty());
        assert!(!cfg.c_green.is_empty()); // others intact
    }

    #[test]
    fn config_toml_parse_basic() {
        let toml = r#"
[cache]
show_cooldown = 3600
[display]
lang = "de"
[updates]
manager = "apt"
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = Config::from_raw(raw);
        assert_eq!(cfg.show_cooldown, 3600);
        assert_eq!(cfg.lang, "de");
        assert!(cfg.cmd_updates.contains("apt list"));
    }

    #[test]
    fn config_toml_unknown_fields_ignored() {
        let toml = r#"
[cache]
show_cooldown = 7200
totally_unknown_key = "whatever"
"#;
        // should not panic — serde ignores unknown fields by default (#[serde(default)])
        let raw: RawConfig = toml::from_str(toml).unwrap();
        assert_eq!(raw.cache.show_cooldown, Some(7200));
    }

    #[test]
    fn config_toml_bad_type_fails_gracefully() {
        // show_cooldown expects u64, passing string — should fail to parse,
        // Config::load() falls back to defaults
        let toml = r#"[cache]
show_cooldown = "not-a-number"
"#;
        assert!(toml::from_str::<RawConfig>(toml).is_err());
    }

    #[test]
    fn expand_replaces_vars() {
        let result = expand("Hello {name}, it is {time}!", &[
            ("name", "Alice"),
            ("time", "morning"),
        ]);
        assert_eq!(result, "Hello Alice, it is morning!");
    }

    #[test]
    fn expand_missing_var_left_as_is() {
        let result = expand("Hello {unknown}!", &[("name", "x")]);
        assert_eq!(result, "Hello {unknown}!");
    }

    #[test]
    fn time_of_day_en() {
        assert_eq!(time_of_day(6,  "en"), "morning");
        assert_eq!(time_of_day(13, "en"), "afternoon");
        assert_eq!(time_of_day(19, "en"), "evening");
        assert_eq!(time_of_day(2,  "en"), "night");
    }

    #[test]
    fn time_of_day_de() {
        assert_eq!(time_of_day(8,  "de"), "Morgen");
        assert_eq!(time_of_day(14, "de"), "Nachmittag");
        assert_eq!(time_of_day(18, "de"), "Abend");
        assert_eq!(time_of_day(23, "de"), "Nacht");
    }

    #[test]
    fn time_of_day_ru() {
        assert_eq!(time_of_day(7,  "ru"), "утро");
        assert_eq!(time_of_day(15, "ru"), "день");
        assert_eq!(time_of_day(20, "ru"), "вечер");
        assert_eq!(time_of_day(3,  "ru"), "ночь");
    }

    #[test]
    fn time_of_day_unknown_lang_falls_back_to_en() {
        assert_eq!(time_of_day(9, "xx"), "morning");
    }
}
