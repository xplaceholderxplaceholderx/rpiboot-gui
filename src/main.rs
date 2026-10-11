//! rpiboot-gui: a macOS front end for Raspberry Pi's usbboot (rpiboot).
//!
//! Three jobs, all driven through usbboot's own tools and shown in a log:
//! - edit a Compute Module's BOOT_ORDER and flash it (recovery/boot.conf ->
//!   update-pieeprom.sh -> rpiboot -d recovery),
//! - mount the module's eMMC on the Mac (rpiboot -d mass-storage-gadget64,
//!   then wait for the disk),
//! - prepare the usbboot checkout (make, rpi-eeprom submodule).

use eframe::egui;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Boot sources the bootloader knows, in the order the app lists them by default.
const SOURCES: [(char, &str); 7] = [
    ('1', "SD card / eMMC"),
    ('6', "NVMe (M.2 / PCIe)"),
    ('4', "USB mass storage"),
    ('2', "Network (TFTP)"),
    ('5', "BCM USB mass storage (CM4 USB-C)"),
    ('3', "RPIBOOT (USB device mode)"),
    ('7', "HTTP"),
];

#[derive(Clone)]
struct Source {
    code: char,
    name: &'static str,
    enabled: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Terminator {
    Restart, // f
    Stop,    // e
}

enum Msg {
    Line(String),
    Done(bool),
}

#[derive(Clone, Copy, PartialEq)]
enum Job {
    Prepare,
    Flash,
    Mount,
}

struct App {
    usbboot: String,
    sources: Vec<Source>,
    terminator: Terminator,
    log: Vec<String>,
    rx: Option<mpsc::Receiver<Msg>>,
    running: Option<Job>,
    last_result: Option<(Job, bool)>,
}

impl Default for App {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let usbboot = home.join("Desktop").join("usbboot").to_string_lossy().into_owned();
        let mut app = App {
            usbboot,
            sources: SOURCES
                .iter()
                .map(|(code, name)| Source { code: *code, name, enabled: *code == '1' || *code == '6' || *code == '4' || *code == '2' })
                .collect(),
            terminator: Terminator::Restart,
            log: Vec::new(),
            rx: None,
            running: None,
            last_result: None,
        };
        app.load_boot_conf();
        app
    }
}

impl App {
    fn usbboot_dir(&self) -> PathBuf {
        PathBuf::from(shellexpand(&self.usbboot))
    }

    fn boot_conf(&self) -> PathBuf {
        self.usbboot_dir().join("recovery").join("boot.conf")
    }

    /// BOOT_ORDER as the firmware wants it: read from the right, so the first
    /// source to try is the last hex digit.
    fn boot_order(&self) -> String {
        let mut digits: Vec<char> = self.sources.iter().filter(|s| s.enabled).map(|s| s.code).collect();
        digits.reverse();
        let term = match self.terminator {
            Terminator::Restart => 'f',
            Terminator::Stop => 'e',
        };
        let mut s = String::from("0x");
        s.push(term);
        s.extend(digits);
        s
    }

    /// Takes the order from a BOOT_ORDER value, keeping unknown digits out.
    fn set_boot_order(&mut self, value: &str) {
        let hex = value.trim().trim_start_matches("0x").trim_start_matches("0X");
        let mut order: Vec<char> = Vec::new();
        for c in hex.chars().rev() {
            match c {
                'f' | 'F' => {
                    self.terminator = Terminator::Restart;
                    break;
                }
                'e' | 'E' => {
                    self.terminator = Terminator::Stop;
                    break;
                }
                '1'..='7' => {
                    if !order.contains(&c) {
                        order.push(c)
                    }
                }
                _ => {}
            }
        }
        let mut sources: Vec<Source> = Vec::new();
        for c in &order {
            if let Some((code, name)) = SOURCES.iter().find(|(code, _)| code == c) {
                sources.push(Source { code: *code, name, enabled: true });
            }
        }
        for (code, name) in SOURCES.iter() {
            if !order.contains(code) {
                sources.push(Source { code: *code, name, enabled: false });
            }
        }
        self.sources = sources;
    }

    fn load_boot_conf(&mut self) {
        if let Ok(text) = std::fs::read_to_string(self.boot_conf()) {
            if let Some(line) = text.lines().find(|l| l.trim_start().starts_with("BOOT_ORDER=")) {
                let value = line.trim_start().trim_start_matches("BOOT_ORDER=").to_string();
                self.set_boot_order(&value);
                self.log.push(format!("boot.conf: BOOT_ORDER={}", value));
            }
        }
    }

    fn status(&self) -> (bool, bool) {
        let dir = self.usbboot_dir();
        let rpiboot = dir.join("rpiboot").is_file();
        let firmware = dir.join("firmware").join("2711").join("pieeprom.bin").metadata().is_ok();
        (rpiboot, firmware)
    }

    fn start(&mut self, job: Job) {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.running = Some(job);
        self.last_result = None;
        let dir = self.usbboot_dir();
        let order = self.boot_order();
        thread::spawn(move || {
            let ok = match job {
                Job::Prepare => prepare(&dir, &tx),
                Job::Flash => flash(&dir, &order, &tx),
                Job::Mount => mount(&dir, &tx),
            };
            let _ = tx.send(Msg::Done(ok));
        });
    }

    fn drain(&mut self) {
        let mut finished = None;
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Line(l) => self.log.push(l),
                    Msg::Done(ok) => finished = Some(ok),
                }
            }
        }
        if let Some(ok) = finished {
            if let Some(job) = self.running.take() {
                self.last_result = Some((job, ok));
                if job == Job::Flash && ok {
                    self.load_boot_conf();
                }
            }
            self.rx = None;
        }
    }
}

fn shellexpand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Runs a command in `cwd`, streaming its output to the log line by line.
/// Returns the exit status and the whole output.
fn run(cwd: &Path, program: &str, args: &[&str], tx: &mpsc::Sender<Msg>) -> (bool, String) {
    let _ = tx.send(Msg::Line(format!("$ (cd {} && {} {})", cwd.display(), program, args.join(" "))));
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Msg::Line(format!("could not start {}: {}", program, e)));
            return (false, String::new());
        }
    };
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let (ltx, lrx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    for pipe in [stdout.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), stderr.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)] {
        if let Some(pipe) = pipe {
            let ltx = ltx.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = ltx.send(line);
                }
            }));
        }
    }
    drop(ltx);
    let mut all = String::new();
    for line in lrx {
        all.push_str(&line);
        all.push('\n');
        let _ = tx.send(Msg::Line(line));
    }
    for r in readers {
        let _ = r.join();
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    (ok, all)
}

fn prepare(dir: &Path, tx: &mpsc::Sender<Msg>) -> bool {
    if !dir.join("Makefile").is_file() {
        let _ = tx.send(Msg::Line(format!("{} is not a usbboot checkout (no Makefile). Clone https://github.com/raspberrypi/usbboot there.", dir.display())));
        return false;
    }
    if !dir.join("firmware").join("2711").join("pieeprom.bin").metadata().is_ok() {
        let _ = tx.send(Msg::Line("Bootloader images missing: fetching the rpi-eeprom submodule.".into()));
        if !run(dir, "git", &["submodule", "update", "--init", "--depth", "1", "rpi-eeprom"], tx).0 {
            return false;
        }
    }
    if !dir.join("rpiboot").is_file() {
        let _ = tx.send(Msg::Line("Building rpiboot (needs libusb and pkg-config from Homebrew).".into()));
        if !run(dir, "make", &[], tx).0 {
            return false;
        }
    }
    let _ = tx.send(Msg::Line("usbboot is ready.".into()));
    true
}

fn flash(dir: &Path, order: &str, tx: &mpsc::Sender<Msg>) -> bool {
    let recovery = dir.join("recovery");
    let conf = recovery.join("boot.conf");
    let text = match std::fs::read_to_string(&conf) {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(Msg::Line(format!("cannot read {}: {}", conf.display(), e)));
            return false;
        }
    };
    let mut replaced = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("BOOT_ORDER=") {
                replaced = true;
                format!("BOOT_ORDER={}", order)
            } else {
                l.to_string()
            }
        })
        .collect();
    if !replaced {
        out.push(format!("BOOT_ORDER={}", order));
    }
    if let Err(e) = std::fs::write(&conf, out.join("\n") + "\n") {
        let _ = tx.send(Msg::Line(format!("cannot write {}: {}", conf.display(), e)));
        return false;
    }
    let _ = tx.send(Msg::Line(format!("boot.conf: BOOT_ORDER={}", order)));
    if !run(&recovery, "./update-pieeprom.sh", &[], tx).0 {
        let _ = tx.send(Msg::Line("update-pieeprom.sh failed (is python3 installed, and the rpi-eeprom submodule fetched? use Prepare usbboot).".into()));
        return false;
    }
    let _ = tx.send(Msg::Line("Waiting for the module: nRPIBOOT jumper fitted, USB connected, then power it.".into()));
    let (ok, output) = run(&recovery, "../rpiboot", &["-d", "."], tx);
    let success = ok && output.contains("\"EEPROM_UPDATE\": \"success\"");
    let _ = tx.send(Msg::Line(if success {
        "EEPROM written. Power-cycle the module.".into()
    } else {
        "The module did not report a successful EEPROM update.".into()
    }));
    success
}

fn mount(dir: &Path, tx: &mpsc::Sender<Msg>) -> bool {
    let _ = tx.send(Msg::Line("Waiting for the module: nRPIBOOT jumper fitted, USB connected, then power it.".into()));
    if !run(dir, "./rpiboot", &["-d", "mass-storage-gadget64"], tx).0 {
        return false;
    }
    let _ = tx.send(Msg::Line("Gadget loaded; waiting for the eMMC to appear as a disk...".into()));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        if let Ok(out) = Command::new("diskutil").args(["list", "external", "physical"]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("/dev/disk") {
                for line in text.lines() {
                    let _ = tx.send(Msg::Line(line.to_string()));
                }
                let _ = tx.send(Msg::Line("The eMMC is mounted; its FAT partition is under /Volumes.".into()));
                return true;
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
    let _ = tx.send(Msg::Line("No disk appeared within 90 s.".into()));
    false
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.running.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let (has_rpiboot, has_firmware) = self.status();
        let busy = self.running.is_some();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("usbboot checkout:");
                ui.add(egui::TextEdit::singleline(&mut self.usbboot).desired_width(360.0));
                ui.label(if has_rpiboot { "rpiboot ✓" } else { "rpiboot missing" });
                ui.label(if has_firmware { "bootloader images ✓" } else { "bootloader images missing" });
                if ui.add_enabled(!busy, egui::Button::new("Prepare usbboot")).clicked() {
                    self.start(Job::Prepare);
                }
            });
            ui.add_space(6.0);
        });

        egui::SidePanel::left("order").min_width(340.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("Boot order");
            ui.label("Tried top to bottom. Unticked sources are left out.");
            ui.add_space(6.0);
            let mut swap: Option<(usize, usize)> = None;
            let count = self.sources.len();
            for i in 0..count {
                ui.horizontal(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new("▲")).clicked() {
                        swap = Some((i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < count, egui::Button::new("▼")).clicked() {
                        swap = Some((i, i + 1));
                    }
                    let s = &mut self.sources[i];
                    ui.checkbox(&mut s.enabled, format!("{}  ({})", s.name, s.code));
                });
            }
            if let Some((a, b)) = swap {
                self.sources.swap(a, b);
            }
            ui.add_space(8.0);
            ui.label("At the end of the list:");
            ui.radio_value(&mut self.terminator, Terminator::Restart, "start over (f)");
            ui.radio_value(&mut self.terminator, Terminator::Stop, "stop (e)");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("BOOT_ORDER =");
                ui.monospace(egui::RichText::new(self.boot_order()).size(20.0));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("Reload from boot.conf")).clicked() {
                    self.load_boot_conf();
                }
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label("Fit the nRPIBOOT jumper, connect the carrier's USB device port, then power the module.");
            ui.add_space(6.0);
            let can_flash = !busy && has_rpiboot && has_firmware && self.sources.iter().any(|s| s.enabled);
            if ui.add_enabled(can_flash, egui::Button::new(egui::RichText::new("Flash boot order").size(16.0))).clicked() {
                self.start(Job::Flash);
            }
            ui.add_space(4.0);
            if ui.add_enabled(!busy && has_rpiboot, egui::Button::new(egui::RichText::new("Mount eMMC").size(16.0))).clicked() {
                self.start(Job::Mount);
            }
            ui.add_space(8.0);
            match (self.running, self.last_result) {
                (Some(Job::Prepare), _) => ui.label("Preparing usbboot..."),
                (Some(Job::Flash), _) => ui.label("Flashing... waiting for the module."),
                (Some(Job::Mount), _) => ui.label("Mounting... waiting for the module."),
                (None, Some((Job::Flash, true))) => ui.colored_label(egui::Color32::from_rgb(95, 215, 167), "Boot order flashed. Power-cycle the module."),
                (None, Some((Job::Mount, true))) => ui.colored_label(egui::Color32::from_rgb(95, 215, 167), "eMMC mounted."),
                (None, Some((Job::Prepare, true))) => ui.colored_label(egui::Color32::from_rgb(95, 215, 167), "usbboot ready."),
                (None, Some((_, false))) => ui.colored_label(egui::Color32::from_rgb(224, 160, 80), "Failed - see the log."),
                (None, None) => ui.label(""),
            };
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Log");
                if ui.button("Clear").clicked() {
                    self.log.clear();
                }
            });
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 620.0]).with_title("rpiboot"),
        ..Default::default()
    };
    eframe::run_native("rpiboot-gui", options, Box::new(|_cc| Ok(Box::new(App::default()))))
}
