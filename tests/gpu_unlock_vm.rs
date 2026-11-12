//! The GPU unlock in a TrueNAS VM (layer 1b): the program runs as a TrueNAS app in a test VM
//! without an NVIDIA GPU and goes through its own task flow, and the VM's kmod, kernel log and
//! reboot show the load order. The checks are the rows of `tests/fixtures/gpu-unlock-vm.tsv`; each
//! runs in case (a), with GPU isolation configured (`/etc/modprobe.d/nvidia.conf` as TrueNAS writes
//! it), and case (b), without it, or once for the VM.
//!
//! The VM has no NVIDIA GPU, so the target is an address with no device behind it,
//! `0000:00:1f.7`. The program only plans the unlock for a supported card it reads in
//! `/sys/bus/pci/devices`, so while a plan is made the test mounts a copy of that directory over it
//! with one more entry at that address (vendor, device and class files only). The kernel never
//! sees a device there: `gpu_unlock` logs `no supported card` and touches nothing.
//!
//! Without `nvidia-smi` readings every update fails its version check. Items 1–4 skip the original,
//! so the new image stays at the permanent path; item 5 saves the original, so the task switches
//! back to it. Before item 5 the test turns the TrueNAS NVIDIA switch off and on, which leaves the
//! file at the permanent path unchanged, then installs an image the program built without the
//! unlock, so the saved original carries none.
//!
//! It needs a disposable TrueNAS 25.10 or 27 VM with a pool, Apps enabled and Internet access, an
//! account with passwordless sudo reached over SSH, and the application image loaded in the VM's
//! Docker. It deletes the app it deploys and the data directory it marked earlier, changes the
//! VM's NVIDIA driver, reboots the VM and turns its NVIDIA switch off and on: never point it at a
//! NAS in use. One run took 15 to 20 minutes on the test VMs. From the repository root:
//!
//! ```text
//! docker build --platform linux/amd64 -t <image> .
//! docker save <image> | ssh <vm> sudo docker load
//! NVM_VM_SSH='ssh -o BatchMode=yes -o ServerAliveInterval=10 <vm>' NVM_VM_IMAGE=<image> \
//!   NVM_VM_DATA=/mnt/<pool>/nvm-vm-test NVM_VM_OUT=<evidence directory> \
//!   cargo test --test gpu_unlock_vm -- --ignored --nocapture
//! ```
//!
//! `NVM_VM_SSH` is a command line split at spaces; the test appends one argument, `sudo -n sh -s`,
//! and writes each script to its standard input. `NVM_VM_DATA` is a plain directory, not a
//! dataset, that does not exist yet or that this test created. The evidence directory receives
//! every task record, the readings behind each check and `results.tsv`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TABLE: &str = include_str!("fixtures/gpu-unlock-vm.tsv");
const VERSION: &str = "610.43.03";
const TARGET: &str = "0000:00:1f.7";
/// GA102, GeForce RTX 3090: in the stable part of the support table.
const TARGET_DEVICE: &str = "0x2204";
const APP: &str = "nvm-vm-test";
const PORT: u16 = 18199;
/// The test's own scratch directory on the VM; a reboot clears it.
const RUN: &str = "/run/nvm-vm-test";
const PCI: &str = "/sys/bus/pci/devices";
const FAKE_PCI: &str = "/run/nvm-vm-test-pci";
const ISOLATION_CONF: &str = "/etc/modprobe.d/nvidia.conf";
/// What TrueNAS writes when a GPU is isolated (25.10 middleware, 26 and 27 upgrade_pyutils).
const ISOLATION_TEXT: &str =
    "softdep nouveau pre: vfio-pci\nsoftdep nvidia pre: vfio-pci\nsoftdep nvidia* pre: vfio-pci\n";
/// Sorts before `gpu-unlock.conf`, so kmod takes its softdep for `nvidia` instead.
const BLOCKER_CONF: &str = "/etc/modprobe.d/aaa-test.conf";
const RULE_PATH: &str = "/usr/lib/modprobe.d/gpu-unlock.conf";
const CANONICAL: &str = "/usr/share/truenas/sysext-extensions/nvidia.raw";
const LINK: &str = "/run/extensions/nvidia.raw";
const TERMINAL: [&str; 4] = ["SUCCEEDED", "STOCK_RESTORED", "FAILED", "INTERRUPTED"];
const TASK_LIMIT: Duration = Duration::from_secs(150 * 60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Case {
    A,
    B,
}

impl Case {
    fn tag(self) -> &'static str {
        match self {
            Case::A => "a",
            Case::B => "b",
        }
    }
}

struct Row {
    item: u32,
    cases: String,
    check: String,
    criterion: String,
}

fn table() -> Vec<Row> {
    TABLE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 4, "table row: {l}");
            assert!(["a", "b", "ab", "vm"].contains(&f[1]), "table row: {l}");
            Row {
                item: f[0].parse().expect("item number"),
                cases: f[1].to_string(),
                check: f[2].to_string(),
                criterion: f[3].to_string(),
            }
        })
        .collect()
}

/// Every (item, case, check) the table asks for; "ab" stands for one in "a" and one in "b".
fn expected(rows: &[Row]) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for r in rows {
        let cases: Vec<String> = match r.cases.as_str() {
            "ab" => vec!["a".into(), "b".into()],
            c => vec![c.into()],
        };
        for c in cases {
            out.push((r.item, c, r.check.clone()));
        }
    }
    out
}

#[test]
fn the_table_names_each_check_once() {
    let all = expected(&table());
    for (i, e) in all.iter().enumerate() {
        assert!(!all[i + 1..].contains(e), "duplicate row {e:?}");
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// One step of `modprobe --show-depends`: the module name with `_` for `-`, and its parameters.
fn steps(show_depends: &str) -> Vec<(String, String)> {
    show_depends
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if let Some(rest) = line.strip_prefix("insmod ") {
                let (path, params) = rest.split_once(' ').unwrap_or((rest, ""));
                let file = path.rsplit('/').next().unwrap_or(path);
                let name = file
                    .trim_end_matches(".xz")
                    .trim_end_matches(".zst")
                    .trim_end_matches(".gz")
                    .trim_end_matches(".ko");
                Some((name.replace('-', "_"), params.trim().to_string()))
            } else {
                line.strip_prefix("builtin ")
                    .map(|name| (name.trim().replace('-', "_"), String::new()))
            }
        })
        .collect()
}

fn index(steps: &[(String, String)], name: &str) -> Option<usize> {
    steps.iter().position(|(n, _)| n == name)
}

/// `a` comes before `b`, and both are listed.
fn before(steps: &[(String, String)], a: &str, b: &str) -> bool {
    matches!((index(steps, a), index(steps, b)), (Some(x), Some(y)) if x < y)
}

/// kmod's `fnmatch` on module names, for the `*` and `?` the softdep patterns here use.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&pattern[1..], name) || (!name.is_empty() && glob(pattern, &name[1..]))
        }
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => glob(&pattern[1..], &name[1..]),
        _ => false,
    }
}

/// The first `softdep` line of `modprobe -c` whose pattern matches `module`, as (pattern, pre).
fn first_softdep(config: &str, module: &str) -> Option<(String, Vec<String>)> {
    let module = module.replace('-', "_");
    config.lines().find_map(|line| {
        let rest = line.strip_prefix("softdep ")?;
        let mut words = rest.split_whitespace();
        let pattern = words.next()?;
        if !glob(pattern.replace('-', "_").as_bytes(), module.as_bytes()) {
            return None;
        }
        let mut pre = Vec::new();
        let mut in_pre = false;
        for w in words {
            match w {
                "pre:" => in_pre = true,
                "post:" => in_pre = false,
                _ if in_pre => pre.push(w.to_string()),
                _ => {}
            }
        }
        Some((pattern.to_string(), pre))
    })
}

/// `key=value` lines up to the first line starting with `---`.
fn facts(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .take_while(|l| !l.starts_with("---"))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

struct Task {
    id: String,
    detail: Value,
    log: Vec<Value>,
}

impl Task {
    fn logged(&self, key: &str) -> bool {
        self.log.iter().any(|l| l["key"] == key)
    }

    fn entered(&self, phase: &str) -> bool {
        self.log
            .iter()
            .any(|l| l["key"] == "log.phase_enter" && l["p"]["phase"] == phase)
    }

    fn phases(&self) -> Vec<String> {
        self.log
            .iter()
            .filter(|l| l["key"] == "log.phase_enter")
            .filter_map(|l| l["p"]["phase"].as_str().map(str::to_string))
            .collect()
    }

    fn status(&self) -> &str {
        self.detail["status"].as_str().unwrap_or("")
    }

    fn error_code(&self) -> &str {
        self.detail["error"]["code"].as_str().unwrap_or("")
    }

    /// The load order of the last check of the update pass.
    fn load_order(&self) -> Value {
        self.detail["checks"]
            .as_array()
            .and_then(|c| c.iter().rev().find(|c| c["pass"] == "update"))
            .map(|c| c["load_order"].clone())
            .unwrap_or(Value::Null)
    }

    fn summary(&self) -> String {
        format!(
            "task {} {} {} phases {}",
            self.id,
            self.status(),
            self.error_code(),
            self.phases().join(",")
        )
    }
}

struct Vm {
    ssh: Vec<String>,
    image: String,
    data: String,
    out: PathBuf,
    truenas: String,
    password: String,
    table: Vec<Row>,
    results: Vec<(u32, String, String, bool, String)>,
}

impl Vm {
    fn from_env() -> Vm {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let ssh: Vec<String> = var("NVM_VM_SSH")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert!(!ssh.is_empty(), "NVM_VM_SSH is empty");
        let data = var("NVM_VM_DATA");
        assert!(
            data.starts_with('/') && !data.contains(char::is_whitespace) && !data.contains('\''),
            "NVM_VM_DATA must be an absolute path without spaces or quotes"
        );
        let image = var("NVM_VM_IMAGE");
        assert!(
            !image.contains(char::is_whitespace) && !image.contains('\''),
            "NVM_VM_IMAGE"
        );
        let out = PathBuf::from(var("NVM_VM_OUT"));
        std::fs::create_dir_all(&out).expect("evidence directory");
        Vm {
            ssh,
            image,
            data,
            out,
            truenas: String::new(),
            password: String::new(),
            table: table(),
            results: Vec::new(),
        }
    }

    fn note(&self, text: &str) {
        eprintln!("[{}] {text}", self.truenas);
    }

    /// One attempt: `script` on the standard input of `sudo -n sh -s` in the VM.
    fn attempt(&self, script: &str) -> std::io::Result<Out> {
        let mut child = Command::new(&self.ssh[0])
            .args(&self.ssh[1..])
            .arg("sudo -n sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin");
        let text = script.to_string();
        let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
        let output = child.wait_with_output()?;
        let _ = writer.join();
        Ok(Out {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// The script, tried again while SSH itself fails (exit 255).
    fn sh(&self, script: &str) -> Out {
        let mut tries = 0;
        loop {
            tries += 1;
            match self.attempt(script) {
                Ok(out) if out.code != 255 => return out,
                Ok(out) if tries >= 6 => panic!("ssh keeps failing: {}", out.stderr),
                Err(e) if tries >= 6 => panic!("ssh: {e}"),
                _ => sleep(Duration::from_secs(10)),
            }
        }
    }

    /// The script's standard output; a non-zero exit stops the test.
    fn ok(&self, script: &str) -> String {
        let out = self.sh(script);
        assert_eq!(
            out.code, 0,
            "script failed ({}):\n{script}\n--- stdout\n{}\n--- stderr\n{}",
            out.code, out.stdout, out.stderr
        );
        out.stdout
    }

    fn save(&self, name: &str, text: &str) {
        std::fs::write(self.out.join(name), text).expect("write evidence");
    }

    /// The table row behind a result: "ab" rows stand for one result in each of "a" and "b".
    fn row(&self, item: u32, case: &str, check: &str) -> Option<&Row> {
        self.table.iter().find(|r| {
            r.item == item
                && r.check == check
                && (r.cases == case || (r.cases == "ab" && case != "vm"))
        })
    }

    fn record(&mut self, item: u32, case: &str, check: &str, pass: bool, detail: String) {
        assert!(
            self.row(item, case, check).is_some(),
            "no table row for item {item} case {case} check {check}"
        );
        self.note(&format!(
            "{} item {item} ({case}) {check}: {detail}",
            if pass { "PASS" } else { "FAIL" }
        ));
        self.results
            .push((item, case.to_string(), check.to_string(), pass, detail));
        self.write_results();
    }

    fn write_results(&self) {
        let mut text = String::from("truenas\titem\tcase\tcheck\tresult\tdetail\tcriterion\n");
        for (item, case, check, pass, detail) in &self.results {
            let criterion = self
                .row(*item, case, check)
                .map_or("", |r| r.criterion.as_str());
            let _ = writeln!(
                text,
                "{}\t{item}\t{case}\t{check}\t{}\t{}\t{criterion}",
                self.truenas,
                if *pass { "PASS" } else { "FAIL" },
                detail.replace(['\t', '\n'], " ")
            );
        }
        self.save("results.tsv", &text);
    }

    // ---- the app and its API ------------------------------------------------------------

    /// One API call through curl in the VM: (HTTP status, JSON body or null).
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let mut s = format!("set -u\nJ={RUN}\nmkdir -p -m 0700 $J\n");
        let mut data = "";
        if let Some(b) = body {
            let _ = writeln!(s, "cat > $J/body <<'NVM_BODY_END'\n{b}\nNVM_BODY_END");
            data = "-H 'Content-Type: application/json' --data-binary @$J/body";
        }
        let _ = writeln!(
            s,
            "code=$(curl -sS -o $J/resp -w '%{{http_code}}' -b $J/cookies -c $J/cookies \
             -H 'Origin: http://127.0.0.1:{PORT}' -H \"X-NVM-Token: $(cat $J/token 2>/dev/null)\" \
             {data} -X {method} 'http://127.0.0.1:{PORT}{path}') || exit 3\n\
             printf '%s\\n' \"$code\"; cat $J/resp; rm -f $J/body $J/resp"
        );
        let out = self.sh(&s);
        if out.code != 0 {
            return (0, json!({"curl": out.stderr}));
        }
        let (code, rest) = out.stdout.split_once('\n').unwrap_or((&out.stdout, ""));
        let value = if rest.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(rest).unwrap_or_else(|_| json!({"text": rest}))
        };
        (code.trim().parse().unwrap_or(0), value)
    }

    /// A call that must succeed; signs in again once when the session is gone.
    fn api_ok(&self, method: &str, path: &str, body: Option<&Value>) -> Value {
        let (mut code, mut value) = self.api(method, path, body);
        if code == 401 {
            self.login();
            (code, value) = self.api(method, path, body);
        }
        assert!(
            (200..300).contains(&code),
            "{method} {path}: {code} {value}"
        );
        value
    }

    fn wait_for(&self, what: &str, test: &str, limit: Duration) {
        let start = Instant::now();
        loop {
            if let Ok(out) = self.attempt(test) {
                if out.code == 0 {
                    return;
                }
            }
            assert!(start.elapsed() < limit, "waited {limit:?} for {what}");
            sleep(Duration::from_secs(5));
        }
    }

    fn wait_app(&self) {
        self.wait_for(
            "the app",
            &format!("curl -fsS -o /dev/null http://127.0.0.1:{PORT}/api/login"),
            Duration::from_secs(600),
        );
    }

    /// Sign in with the password the test chose; the app may still be restarting with Docker.
    fn login(&self) {
        for _ in 0..6 {
            self.wait_app();
            if self.try_login() {
                return;
            }
            sleep(Duration::from_secs(10));
        }
        panic!("cannot sign in to the app");
    }

    fn try_login(&self) -> bool {
        let out = self.sh(&format!(
            "set -eu\nJ={RUN}\nmkdir -p -m 0700 $J\numask 077\nrm -f $J/cookies\n\
             printf '{{\"username\":\"admin\",\"password\":\"%s\"}}' '{pw}' > $J/body\n\
             code=$(curl -sS -o $J/resp -w '%{{http_code}}' -c $J/cookies \
             -H 'Origin: http://127.0.0.1:{PORT}' -H 'Content-Type: application/json' \
             --data-binary @$J/body -X POST http://127.0.0.1:{PORT}/api/login)\n\
             rm -f $J/body\n[ \"$code\" = 200 ] || {{ cat $J/resp >&2; exit 3; }}\n\
             python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"token\"], end=\"\")' $J/resp > $J/token\n\
             rm -f $J/resp\necho signed-in",
            pw = self.password
        ));
        out.code == 0 && out.stdout.contains("signed-in")
    }

    /// Delete the test's app and data directory if present, deploy the image as a custom app,
    /// create the administrator from the setup code in the app log and sign in.
    fn deploy(&mut self) {
        let compose = json!({
            "app_name": APP,
            "custom_app": true,
            "custom_compose_config": {"services": {"nvm": {
                "image": self.image,
                "restart": "unless-stopped",
                "privileged": true,
                "pid": "host",
                "runtime": "runc",
                // A slow VM network can make the download and build outlast the default 30.
                "environment": ["NVM_BUILD_TIMEOUT_MINUTES=120"],
                "volumes": [format!("{}:/var/lib/nvm", self.data)],
                "ports": [format!("127.0.0.1:{PORT}:8199")],
            }}},
        });
        self.ok(&format!(
            "set -eu\nD='{data}'\n\
             if systemctl list-units --no-legend --state=active,activating 'nvm-task-*' | grep -q .; then \
               echo 'a task unit is still active' >&2; exit 3; fi\n\
             n=$(midclt call app.query '[[\"name\",\"=\",\"{APP}\"]]' | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')\n\
             if [ \"$n\" != 0 ]; then midclt call --job app.delete {APP} '{{\"remove_images\": false, \"remove_ix_volumes\": true}}' >/dev/null; fi\n\
             if [ -e \"$D\" ]; then [ -f \"$D/.nvm-vm-test\" ] || {{ echo \"$D exists and is not this test's\" >&2; exit 3; }}; rm -rf \"$D\"; fi\n\
             mkdir -p \"$D\"; touch \"$D/.nvm-vm-test\"\n\
             docker image inspect '{image}' >/dev/null\n\
             rm -rf {RUN}; mkdir -m 0700 {RUN}\n\
             cat > {RUN}/create.json <<'NVM_BODY_END'\n{compose}\nNVM_BODY_END\n\
             midclt call --job app.create \"$(cat {RUN}/create.json)\" >/dev/null\nrm -f {RUN}/create.json",
            data = self.data,
            image = self.image,
        ));
        self.wait_app();
        let password = self.ok(&format!(
            "set -eu\nJ={RUN}\numask 077\n\
             cid=$(docker ps -q --filter label=com.docker.compose.project=ix-{APP})\n\
             code=$(docker logs \"$cid\" 2>&1 | grep -o 'SETUP CODE: [0-9A-Z]\\{{4\\}}\\(-[0-9A-Z]\\{{4\\}}\\)\\{{3\\}}' | tail -1 | cut -d' ' -f3)\n\
             [ -n \"$code\" ] || {{ echo 'no setup code in the app log' >&2; exit 3; }}\n\
             pw=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n')\n\
             printf '{{\"code\":\"%s\",\"new_password\":\"%s\"}}' \"$code\" \"$pw\" > $J/body\n\
             status=$(curl -sS -o $J/resp -w '%{{http_code}}' -c $J/cookies \
             -H 'Origin: http://127.0.0.1:{PORT}' -H 'Content-Type: application/json' \
             --data-binary @$J/body -X POST http://127.0.0.1:{PORT}/api/setup)\n\
             rm -f $J/body\n[ \"$status\" = 200 ] || {{ cat $J/resp >&2; exit 3; }}\n\
             python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"token\"], end=\"\")' $J/resp > $J/token\n\
             rm -f $J/resp\nprintf '%s' \"$pw\""
        ));
        assert_eq!(password.len(), 48, "generated password");
        self.password = password;
        let home = self.api_ok("GET", "/api/home", None);
        self.save("deploy-home.json", &format!("{home:#}"));
        assert!(
            home["link"]["points_to_canonical"] == true,
            "the NVIDIA link must point to the permanent path: {}",
            home["link"]
        );
    }

    // ---- the VM ------------------------------------------------------------------------

    /// Turn the TrueNAS NVIDIA switch (test preparation only; the program never does this).
    fn switch(&self, on: bool) {
        let call = if self.truenas.starts_with("25.") {
            format!(
                "midclt call --job {} '{{\"nvidia\": {on}}}'",
                concat!("docker", ".update")
            )
        } else {
            format!(
                "midclt call {} '{{\"nvidia\": {on}}}'",
                concat!("system.advanced", ".update")
            )
        };
        let state = if on { "-L" } else { "! -e" };
        self.ok(&format!(
            "set -eu\n{call} >/dev/null\n\
             for i in $(seq 1 60); do [ {state} {LINK} ] && exit 0; sleep 2; done\n\
             echo 'the link did not follow the switch' >&2; exit 3"
        ));
        // Docker restarts with the switch; give it time to stop the app before waiting for it.
        // Before the deployment there is no app of this run to sign in to.
        if !self.password.is_empty() {
            sleep(Duration::from_secs(20));
            self.login();
        }
    }

    fn reboot(&self, why: &str) {
        let boot = self.ok("cat /proc/sys/kernel/random/boot_id");
        self.note(&format!("reboot: {why}"));
        // Once only: a retry after the connection dropped could reboot the new boot again.
        let _ = self.attempt(&format!("midclt call system.reboot '{why}' >/dev/null"));
        self.wait_for(
            "the reboot",
            &format!(
                "[ \"$(cat /proc/sys/kernel/random/boot_id)\" != '{}' ]",
                boot.trim()
            ),
            Duration::from_secs(1200),
        );
        self.wait_for(
            "middleware",
            "midclt call system.ready | grep -qix true",
            Duration::from_secs(900),
        );
        self.login();
    }

    /// Case (a) writes TrueNAS's isolation file, case (b) deletes it; `blocker` is the content of
    /// the file sorted before the image's rule, or none.
    fn modprobe_files(&self, case: Case, blocker: Option<&str>) {
        let mut s = String::from("set -eu\n");
        match case {
            Case::A => {
                let _ = writeln!(s, "printf '%s' '{ISOLATION_TEXT}' > {ISOLATION_CONF}");
            }
            Case::B => {
                let _ = writeln!(s, "rm -f {ISOLATION_CONF}");
            }
        }
        match blocker {
            Some(line) => {
                let _ = writeln!(s, "printf '%s\\n' '{line}' > {BLOCKER_CONF}");
            }
            None => {
                let _ = writeln!(s, "rm -f {BLOCKER_CONF}");
            }
        }
        self.ok(&s);
    }

    /// Mount a copy of the PCI device directory with an NVIDIA entry at the target over the real
    /// one, or take it away again.
    fn fake_gpu(&self, on: bool) {
        let script = if on {
            format!(
                "set -eu\n\
                 [ -z \"$(findmnt -n -o SOURCE --mountpoint {PCI})\" ] || {{ echo 'already mounted' >&2; exit 3; }}\n\
                 [ ! -e '{PCI}/{TARGET}' ] || {{ echo 'a device exists at the target' >&2; exit 3; }}\n\
                 mkdir -p {FAKE_PCI}; mount -t tmpfs -o mode=0755 nvm-vm-test-pci {FAKE_PCI}\n\
                 for e in {PCI}/*; do ln -s \"$(readlink \"$e\")\" \"{FAKE_PCI}/${{e##*/}}\"; done\n\
                 mkdir '{FAKE_PCI}/{TARGET}'\n\
                 printf '0x10de\\n' > '{FAKE_PCI}/{TARGET}/vendor'\n\
                 printf '{TARGET_DEVICE}\\n' > '{FAKE_PCI}/{TARGET}/device'\n\
                 printf '0x030000\\n' > '{FAKE_PCI}/{TARGET}/class'\n\
                 mount --bind {FAKE_PCI} {PCI}"
            )
        } else {
            format!(
                "set -eu\n\
                 if [ \"$(findmnt -n -o SOURCE --mountpoint {PCI})\" = nvm-vm-test-pci ]; then umount {PCI}; fi\n\
                 if mountpoint -q {FAKE_PCI}; then umount {FAKE_PCI}; fi\nrmdir {FAKE_PCI} 2>/dev/null || true"
            )
        };
        self.ok(&script);
    }

    /// Make a plan for `source`, check its confirmation, submit it with `button` and wait for the
    /// end. `button` "keep" means "save" when offered and otherwise "start" (an original is saved).
    fn update(&mut self, label: &str, source: Value, button: &str) -> Task {
        let local = source["kind"] == "local";
        if !local {
            self.fake_gpu(true);
        }
        let plan = self.api_ok("POST", "/api/plans", Some(&json!({"source": source})));
        self.save(&format!("{label}-plan.json"), &format!("{plan:#}"));
        let plan_id = plan["plan_id"].as_str().unwrap_or_default().to_string();
        assert!(
            plan["result"] == "ready" && !plan_id.is_empty(),
            "plan {label}: {plan}"
        );
        if source["gpu_unlock"] == true {
            let warnings = plan["dialog"]["confirmation"]["warnings"]
                .as_array()
                .cloned();
            let line = warnings
                .unwrap_or_default()
                .into_iter()
                .find(|w| w["key"] == "confirm.gpu_unlock");
            assert_eq!(
                line.map(|w| w["params"]["targets"].clone()),
                Some(json!([TARGET])),
                "plan {label}: the confirmation lists the target"
            );
        }
        let offered: Vec<String> = plan["dialog"]["confirmation"]["buttons"]
            .as_array()
            .map(|b| {
                b.iter()
                    .filter(|b| b["enabled"] == true)
                    .filter_map(|b| b["button"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let button = match button {
            "keep" if offered.iter().any(|b| b == "save") => "save",
            "keep" => "start",
            b => b,
        };
        assert!(
            offered.iter().any(|b| b == button),
            "plan {label}: {button} not among {offered:?}"
        );
        let submitted = self.api_ok(
            "POST",
            "/api/tasks",
            Some(&json!({"plan_id": plan_id, "button": button})),
        );
        if !local {
            self.fake_gpu(false);
        }
        let id = submitted["task_id"].as_str().expect("task_id").to_string();
        self.note(&format!("{label}: task {id} ({button})"));
        self.wait_task(label, &id)
    }

    fn wait_task(&self, label: &str, id: &str) -> Task {
        let start = Instant::now();
        let mut log: Vec<Value> = Vec::new();
        let mut after = 0u64;
        let mut phase = String::new();
        loop {
            let detail = self.api_ok("GET", &format!("/api/tasks/{id}?after={after}"), None);
            if detail["log"]["reset"] == true {
                log.clear();
            }
            let lines = detail["log"]["lines"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let drained = lines.is_empty();
            log.extend(lines);
            after = detail["log"]["next"].as_u64().unwrap_or(after);
            let status = detail["status"].as_str().unwrap_or("").to_string();
            let now = detail["phase"].as_str().unwrap_or("").to_string();
            if now != phase {
                self.note(&format!("{label}: {status} {now}"));
                phase = now;
            }
            if TERMINAL.contains(&status.as_str()) && drained {
                let task = Task {
                    id: id.to_string(),
                    detail,
                    log,
                };
                self.save(&format!("{label}-task.json"), &format!("{:#}", task.detail));
                self.save(
                    &format!("{label}-log.json"),
                    &format!("{:#}", Value::Array(task.log.clone())),
                );
                self.note(&format!("{label}: {}", task.summary()));
                return task;
            }
            assert!(
                start.elapsed() < TASK_LIMIT,
                "task {id} still {status} after {TASK_LIMIT:?}"
            );
            if !TERMINAL.contains(&status.as_str()) {
                sleep(Duration::from_secs(15));
            }
        }
    }

    /// The facts of a local driver row's image: its nvidia.ko and gpu_unlock.ko.
    fn image_facts(&self, row: &str) -> std::collections::BTreeMap<String, String> {
        let text = self.ok(&format!(
            "set -eu\nR='{data}/drivers/{row}/nvidia.raw'\nK=$(uname -r)\n\
             T=$(mktemp -d {RUN}/image.XXXXXX)\n\
             L=$(unsquashfs -l \"$R\" | sed -n \"s#^squashfs-root/##p\")\n\
             NV=$(printf '%s\\n' \"$L\" | grep -F \"usr/lib/modules/$K/\" | grep '/nvidia\\.ko$' | head -1)\n\
             GU=\"usr/lib/modules/$K/extra/gpu_unlock.ko\"\n\
             printf '%s\\n' \"$L\" | grep -qxF \"$GU\" || GU=\n\
             unsquashfs -no-progress -no-xattrs -d \"$T/x\" \"$R\" \"$NV\" $GU >/dev/null\n\
             echo \"kernel=$K\"\necho \"size=$(stat -c %s \"$R\")\"\necho \"nvidia_path=$NV\"\n\
             echo \"nvidia_version=$(modinfo -F version \"$T/x/$NV\")\"\n\
             echo \"nvidia_vermagic=$(modinfo -F vermagic \"$T/x/$NV\")\"\n\
             echo \"nvidia_versions=$(modprobe --dump-modversions \"$T/x/$NV\" 2>/dev/null | wc -l)\"\n\
             if [ -n \"$GU\" ]; then\n\
               echo \"gpu_unlock_vermagic=$(modinfo -F vermagic \"$T/x/$GU\")\"\n\
               echo \"gpu_unlock_versions=$(modprobe --dump-modversions \"$T/x/$GU\" 2>/dev/null | wc -l)\"\n\
             fi\n\
             m=$(grep -h '^CONFIG_MODVERSIONS=' /usr/src/linux-headers-truenas-production-amd64/include/config/auto.conf \"/boot/config-$K\" 2>/dev/null | head -1)\n\
             echo \"modversions=${{m#CONFIG_MODVERSIONS=}}\"\nrm -rf \"$T\"",
            data = self.data
        ));
        self.save(&format!("image-{row}.txt"), &text);
        facts(&text)
    }

    fn gpu_unlock_loaded(&self) -> bool {
        self.sh("grep -q '^gpu_unlock ' /proc/modules").code == 0
    }

    fn sha256(&self, path: &str) -> String {
        self.ok(&format!("sha256sum '{path}' | cut -d' ' -f1"))
            .trim()
            .to_string()
    }

    // ---- the items -----------------------------------------------------------------------

    fn item_0(&mut self) {
        let text = self.ok(
            "set -u\necho \"truenas=$(cat /etc/version)\"\necho \"kernel=$(uname -r)\"\n\
             echo \"modprobe=$(modprobe --version | head -1)\"\necho \"lib=$(readlink -f /lib)\"\n\
             echo \"unsquashfs=$(unsquashfs -version 2>&1 | head -1)\"\necho ---\nls -ld /lib",
        );
        self.save("env.txt", &text);
        let f = facts(&text);
        self.truenas = f.get("truenas").cloned().unwrap_or_default();
        assert!(!self.truenas.is_empty(), "TrueNAS version");
        let lib = f.get("lib").cloned().unwrap_or_default();
        self.record(
            0,
            "vm",
            "lib_is_usr_lib",
            lib == "/usr/lib",
            format!(
                "/lib -> {lib}; {}; kernel {}",
                f.get("modprobe").map_or("", String::as_str),
                f.get("kernel").map_or("", String::as_str)
            ),
        );
    }

    /// Items 1 and 2: one update with the unlock from the official download, without saving the
    /// original. Returns its local driver row.
    fn item_1_2(&mut self, case: Case) -> String {
        let c = case.tag();
        assert!(
            !self.gpu_unlock_loaded(),
            "item 1 needs a kernel without gpu_unlock"
        );
        let task = self.update(
            &format!("{c}-1"),
            json!({"kind": "official", "version": VERSION, "gpus": [TARGET], "gpu_unlock": true}),
            "skip",
        );
        self.record(
            1,
            c,
            "firmware_generated",
            task.entered("unlock_firmware")
                && task.logged("log.unlock_firmware_generated")
                && !task.logged("log.unlock_firmware_reused"),
            format!(
                "phase unlock_firmware {}, generated {}, reused {}",
                task.entered("unlock_firmware"),
                task.logged("log.unlock_firmware_generated"),
                task.logged("log.unlock_firmware_reused")
            ),
        );
        self.record(
            1,
            c,
            "module_test_loaded",
            task.entered("unlock_preload") && task.logged("log.unlock_preloaded"),
            format!(
                "phase unlock_preload {}, preloaded {}, skipped {}",
                task.entered("unlock_preload"),
                task.logged("log.unlock_preloaded"),
                task.logged("log.unlock_preload_skipped")
            ),
        );
        let row = format!("{}-build", task.id);
        let rows = self.api_ok("GET", "/api/drivers", None);
        self.save(&format!("{c}-1-drivers.json"), &format!("{rows:#}"));
        let delivered = rows["rows"]
            .as_array()
            .and_then(|r| r.iter().find(|r| r["id"] == row.as_str()).cloned())
            .unwrap_or(Value::Null);
        self.record(
            1,
            c,
            "built_and_delivered",
            task.entered("build")
                && task.entered("unload")
                && !task.error_code().starts_with("BUILD")
                && delivered["version"] == VERSION
                && delivered["gpu_unlock_targets"] == json!([TARGET]),
            format!(
                "{}; row {row} version {} targets {}",
                task.summary(),
                delivered["version"],
                delivered["gpu_unlock_targets"]
            ),
        );
        let image = self.image_facts(&row);
        let get = |k: &str| image.get(k).cloned().unwrap_or_default();
        let kernel = get("kernel");
        self.record(
            1,
            c,
            "nvidia_610",
            get("nvidia_version") == VERSION
                && get("nvidia_vermagic").starts_with(&format!("{kernel} ")),
            format!(
                "{} version {} vermagic {}",
                get("nvidia_path"),
                get("nvidia_version"),
                get("nvidia_vermagic")
            ),
        );
        let want = get("modversions") == "y";
        let has = |k: &str| get(k).parse::<u64>().unwrap_or(0) > 0;
        self.record(
            1,
            c,
            "modversions",
            has("nvidia_versions") == want && has("gpu_unlock_versions") == want,
            format!(
                "CONFIG_MODVERSIONS={}; __versions entries: nvidia.ko {}, gpu_unlock.ko {}",
                get("modversions"),
                get("nvidia_versions"),
                get("gpu_unlock_versions")
            ),
        );

        let text = self.ok(&format!(
            "set -u\nK=$(uname -r)\n\
             echo \"sysext=$(systemd-sysext status --json=short)\"\n\
             echo \"canonical=$(sha256sum {CANONICAL} | cut -d' ' -f1)\"\n\
             echo \"row=$(sha256sum '{data}/drivers/{row}/nvidia.raw' | cut -d' ' -f1)\"\n\
             echo \"rule=$([ -f {RULE_PATH} ] && echo yes || echo no)\"\n\
             echo \"module=$([ -f /usr/lib/modules/$K/extra/gpu_unlock.ko ] && echo yes || echo no)\"\n\
             echo \"firmware=$(ls /usr/lib/firmware/gpu-unlock/{VERSION} 2>/dev/null | wc -l)\"\n\
             echo \"targets=$(cat /sys/module/gpu_unlock/parameters/targets 2>&1)\"\n\
             echo ---rule; cat {RULE_PATH}",
            data = self.data
        ));
        self.save(&format!("{c}-2-host.txt"), &text);
        let host = facts(&text);
        let h = |k: &str| host.get(k).cloned().unwrap_or_default();
        let sysext: Value = serde_json::from_str(&h("sysext")).unwrap_or(Value::Null);
        let merged = sysext.as_array().is_some_and(|rows| {
            rows.iter().any(|r| {
                r["hierarchy"] == "/usr"
                    && r["extensions"]
                        .as_array()
                        .is_some_and(|e| e.iter().any(|e| e == "nvidia"))
            })
        });
        let in_place = !h("row").is_empty() && h("canonical") == h("row");
        self.record(
            2,
            c,
            "merged",
            merged && in_place,
            format!(
                "sysext {}; permanent path {} row {}",
                h("sysext"),
                h("canonical"),
                h("row")
            ),
        );
        self.record(
            2,
            c,
            "files",
            h("rule") == "yes" && h("module") == "yes" && h("firmware") == "16",
            format!(
                "gpu-unlock.conf {}, extra/gpu_unlock.ko {}, firmware files {}",
                h("rule"),
                h("module"),
                h("firmware")
            ),
        );
        self.record(
            2,
            c,
            "targets_null",
            h("targets") == "(null)",
            format!("targets reads {}", h("targets")),
        );
        let order = task.load_order();
        self.record(
            2,
            c,
            "record_both",
            order["vfio_pci_first"] == true && order["gpu_unlock_before_nvidia"] == true,
            format!(
                "vfio_pci_first {} gpu_unlock_before_nvidia {} nvidia_conf {}",
                order["vfio_pci_first"], order["gpu_unlock_before_nvidia"], order["nvidia_conf"]
            ),
        );
        let reading = steps(order["show_depends"]["value"].as_str().unwrap_or(""));
        if case == Case::A {
            self.record(
                2,
                c,
                "record_vfio_line",
                before(&reading, "vfio_pci", "nvidia"),
                format!("reading {reading:?}"),
            );
        }
        self.record(
            2,
            c,
            "skip_outcome",
            task.status() == "FAILED"
                && task.error_code() == "NO_USABLE_ORIGINAL"
                && task.detail["restore"]["decision"] == "none"
                && in_place,
            format!(
                "{} {} restore {}; the new image is at the permanent path: {in_place}",
                task.status(),
                task.error_code(),
                task.detail["restore"]
            ),
        );
        row
    }

    /// Item 3: middleware loads the driver at boot.
    fn item_3(&mut self, case: Case) {
        let c = case.tag();
        self.reboot("gpu-unlock VM test: item 3");
        if case == Case::A {
            let text = self.sh(&format!("cat {ISOLATION_CONF}")).stdout;
            self.record(
                3,
                c,
                "isolation_file_kept",
                text == ISOLATION_TEXT,
                format!("after the reboot: {text:?}"),
            );
        }
        let journal = self.ok("journalctl -k -b -o cat --no-pager");
        self.save(&format!("{c}-3-journal.txt"), &journal);
        let line = format!("gpu_unlock: no supported card at \"{TARGET}\"");
        let lines: Vec<&str> = journal.lines().collect();
        let unlock = lines.iter().position(|l| l.contains(&line));
        let nvrm = lines.iter().position(|l| l.contains("NVRM"));
        self.record(
            3,
            c,
            "boot_line_first",
            matches!((unlock, nvrm), (Some(u), Some(n)) if u < n),
            format!("kernel log line {unlock:?} for gpu_unlock, first NVRM line {nvrm:?}"),
        );
        let text = self.ok(
            "set -u\necho \"gpu_unlock=$([ -d /sys/module/gpu_unlock ] && echo yes || echo no)\"\n\
             echo \"vfio_pci=$([ -d /sys/module/vfio_pci ] && echo yes || echo no)\"\n\
             echo \"targets=$(cat /sys/module/gpu_unlock/parameters/targets 2>&1)\"\n\
             echo ---modules; grep -E '^(gpu_unlock|vfio|nvidia)' /proc/modules || true",
        );
        self.save(&format!("{c}-3-modules.txt"), &text);
        let m = facts(&text);
        let g = |k: &str| m.get(k).cloned().unwrap_or_default();
        self.record(
            3,
            c,
            "modules_loaded",
            g("gpu_unlock") == "yes" && g("vfio_pci") == "yes" && g("targets") == TARGET,
            format!(
                "gpu_unlock {} (targets {}), vfio_pci {}",
                g("gpu_unlock"),
                g("targets"),
                g("vfio_pci")
            ),
        );
        let depends = self.ok("modprobe --show-depends nvidia");
        self.save(&format!("{c}-3-show-depends.txt"), &depends);
        let s = steps(&depends);
        let params = index(&s, "gpu_unlock").map(|i| s[i].1.clone());
        self.record(
            3,
            c,
            "show_depends_order",
            before(&s, "vfio_pci", "gpu_unlock")
                && before(&s, "gpu_unlock", "nvidia")
                && params.as_deref() == Some(format!("targets={TARGET}").as_str()),
            format!("{s:?}"),
        );
        let text = self.ok(&format!(
            "set -u\ncount() {{ journalctl -k -b -o cat --no-pager | grep -cF '{line}'; }}\n\
             echo \"before=$(count)\"\nunloaded=\n\
             for m in nvidia_uvm nvidia_drm nvidia_modeset nvidia; do \
               if grep -q \"^$m \" /proc/modules; then rmmod $m && unloaded=\"$unloaded $m\"; fi; done\n\
             echo \"unloaded=$unloaded\"\n\
             out=$(modprobe nvidia 2>&1); echo \"modprobe=$? $out\"\nsleep 2\necho \"after=$(count)\""
        ));
        self.save(&format!("{c}-3-second-load.txt"), &text);
        let f = facts(&text);
        let n = |k: &str| f.get(k).and_then(|v| v.trim().parse::<u32>().ok());
        self.record(
            3,
            c,
            "no_second_load",
            n("before") == Some(1) && n("after") == Some(1),
            text.replace('\n', "; "),
        );
        let home = self.api_ok("GET", "/api/home", None);
        self.save(&format!("{c}-3-home.json"), &format!("{home:#}"));
        let cards = &home["gpu_unlock"]["value"]["cards"];
        let card = &cards[0];
        self.record(
            3,
            c,
            "home_reading",
            cards.as_array().map(Vec::len) == Some(1)
                && card["address"] == TARGET
                && card["result"] == "not_unlocked"
                && card["line"].as_str().is_some_and(|l| l.contains(&line)),
            format!("{}", home["gpu_unlock"]),
        );
    }

    /// Item 4: the isolation rule stays in force.
    fn item_4(&mut self, case: Case) {
        let c = case.tag();
        let config = self.ok("modprobe -c | grep '^softdep ' || true");
        self.save(&format!("{c}-4-softdeps.txt"), &config);
        match case {
            Case::A => {
                let nvidia = first_softdep(&config, "nvidia");
                self.record(
                    4,
                    c,
                    "nvidia_rule_is_image",
                    nvidia
                        == Some((
                            "nvidia".to_string(),
                            vec!["vfio-pci".to_string(), "gpu_unlock".to_string()],
                        )),
                    format!("first softdep for nvidia: {nvidia:?}"),
                );
                let mut pass = true;
                let mut detail = String::new();
                for module in ["nvidia_drm", "nvidia_uvm"] {
                    let rule = first_softdep(&config, module);
                    let depends = self.ok(&format!("modprobe --show-depends {module}"));
                    self.save(&format!("{c}-4-show-depends-{module}.txt"), &depends);
                    let s = steps(&depends);
                    pass &= rule == Some(("nvidia*".to_string(), vec!["vfio-pci".to_string()]))
                        && before(&s, "vfio_pci", "nvidia")
                        && before(&s, "vfio_pci", module);
                    let _ = write!(detail, "{module}: rule {rule:?}, {s:?}; ");
                }
                self.record(4, c, "other_modules_vfio", pass, detail);
                let loaded = self.sh("[ -d /sys/module/vfio_pci ]").code == 0;
                self.record(
                    4,
                    c,
                    "vfio_loaded",
                    loaded,
                    format!("/sys/module/vfio_pci {loaded}"),
                );
            }
            Case::B => {
                let text = self.ok(
                    "set -u\necho \"driver=$([ -d /sys/bus/pci/drivers/vfio-pci ] && echo yes || echo no)\"\n\
                     echo \"bound=$(ls /sys/bus/pci/drivers/vfio-pci 2>/dev/null | grep -c '^[0-9a-f]\\{4\\}:' || true)\"",
                );
                self.save(&format!("{c}-4-vfio.txt"), &text);
                let f = facts(&text);
                self.record(
                    4,
                    c,
                    "vfio_unbound",
                    f.get("driver").map(String::as_str) == Some("yes")
                        && f.get("bound").map(String::as_str) == Some("0"),
                    text.replace('\n', "; "),
                );
            }
        }
    }

    /// The permanent path equals the saved original byte for byte and the rule is not merged.
    fn restored(&self, original: &str) -> (bool, String) {
        let text = self.ok(&format!(
            "set -u\necho \"same=$(cmp -s {CANONICAL} '{data}/drivers/{original}/nvidia.raw' && echo yes || echo no)\"\n\
             echo \"rule=$([ -e {RULE_PATH} ] && echo present || echo gone)\"",
            data = self.data
        ));
        let f = facts(&text);
        let same = f.get("same").map(String::as_str) == Some("yes");
        let gone = f.get("rule").map(String::as_str) == Some("gone");
        (same && gone, text.replace('\n', "; "))
    }

    /// Item 5: the load-order items with a blocking file, each update saving (or keeping) the
    /// original, which carries no unlock.
    fn item_5(&mut self, unlock_row: &str, plain_row: &str) {
        let before_switch = self.sha256(CANONICAL);
        self.switch(false);
        self.switch(true);
        let after_switch = self.sha256(CANONICAL);
        let rule = self.sh(&format!("[ -f {RULE_PATH} ]")).code == 0;
        self.record(
            5,
            "vm",
            "switch_keeps_file",
            before_switch == after_switch && rule,
            format!("before {before_switch} after {after_switch}; gpu-unlock.conf merged {rule}"),
        );

        // An image without the unlock at the permanent path, so the original saved next has none.
        let plain = self.update(
            "5-plain",
            json!({"kind": "local", "row_id": plain_row}),
            "skip",
        );
        let (ok, detail) = self.restored(plain_row);
        assert!(ok, "5-plain: {} {detail}", plain.summary());

        for case in [Case::A, Case::B] {
            let c = case.tag();
            for (run, blocker) in [
                (1, "softdep nvidia pre: vfio-pci"),
                (2, "softdep nvidia pre: gpu_unlock"),
            ] {
                self.modprobe_files(case, Some(blocker));
                let task = self.update(
                    &format!("{c}-5-{run}"),
                    json!({"kind": "local", "row_id": unlock_row}),
                    "keep",
                );
                let order = task.load_order();
                let reading = steps(order["show_depends"]["value"].as_str().unwrap_or(""));
                let detail = format!(
                    "{}; vfio_pci_first {} gpu_unlock_before_nvidia {} nvidia_conf {}; reading {reading:?}",
                    task.summary(),
                    order["vfio_pci_first"],
                    order["gpu_unlock_before_nvidia"],
                    order["nvidia_conf"]
                );
                if run == 1 {
                    self.record(
                        5,
                        c,
                        "r1_record",
                        order["gpu_unlock_before_nvidia"] == false
                            && index(&reading, "gpu_unlock").is_none()
                            && index(&reading, "nvidia").is_some()
                            && order["vfio_pci_first"] == true,
                        detail.clone(),
                    );
                    if case == Case::A {
                        self.record(
                            5,
                            c,
                            "r1_vfio_line",
                            before(&reading, "vfio_pci", "nvidia"),
                            detail,
                        );
                    }
                } else {
                    self.record(
                        5,
                        c,
                        "r2_record",
                        index(&reading, "vfio_pci").is_none()
                            && index(&reading, "nvidia").is_some()
                            && order["vfio_pci_first"] == (case == Case::B),
                        detail,
                    );
                }
                let rows = self.api_ok("GET", "/api/drivers", None);
                let original: Vec<String> = rows["rows"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .filter(|r| r["kind"] == "original")
                            .filter_map(|r| r["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(original.len(), 1, "one saved original: {rows}");
                let (ok, detail) = self.restored(&original[0]);
                let check = if run == 1 {
                    "r1_restored"
                } else {
                    "r2_restored"
                };
                self.record(
                    5,
                    c,
                    check,
                    ok,
                    format!("{}; original {}; {detail}", task.summary(), original[0]),
                );
            }
        }
    }

    /// Every row of the table has a result; a missing one counts as failed.
    fn finish(&mut self) {
        let want = expected(&self.table);
        for (item, case, check) in &want {
            if !self
                .results
                .iter()
                .any(|r| r.0 == *item && &r.1 == case && &r.2 == check)
            {
                self.results
                    .push((*item, case.clone(), check.clone(), false, "not run".into()));
            }
        }
        self.write_results();
        let failed: Vec<String> = self
            .results
            .iter()
            .filter(|r| !r.3)
            .map(|r| format!("item {} ({}) {}: {}", r.0, r.1, r.2, r.4))
            .collect();
        assert!(failed.is_empty(), "failed checks:\n{}", failed.join("\n"));
    }
}

/// The whole run on one VM; see the file header.
#[test]
#[ignore = "needs a disposable TrueNAS VM named by NVM_VM_SSH: see the file header"]
fn the_gpu_unlock_in_a_truenas_vm() {
    let mut vm = Vm::from_env();
    eprintln!("evidence: {}", vm.out.display());
    vm.item_0();
    vm.fake_gpu(false);
    if vm.sh(&format!("[ -L {LINK} ]")).code != 0 {
        vm.note("turning the NVIDIA switch on");
        vm.switch(true);
    }
    vm.deploy();

    // An image without the unlock first: rebooting with it gives a kernel without gpu_unlock, and
    // item 5 later installs it again before saving the original.
    let plain = vm.update(
        "plain",
        json!({"kind": "official", "version": VERSION, "gpus": [TARGET], "gpu_unlock": false}),
        "skip",
    );
    let plain_row = format!("{}-build", plain.id);
    let (ok, detail) = vm.restored(&plain_row);
    assert!(ok, "plain: {} {detail}", plain.summary());
    vm.reboot("gpu-unlock VM test: start without gpu_unlock");

    let mut unlock_row = String::new();
    for case in [Case::A, Case::B] {
        if case == Case::B {
            let back = vm.update(
                "b-0-plain",
                json!({"kind": "local", "row_id": plain_row}),
                "skip",
            );
            let (ok, detail) = vm.restored(&plain_row);
            assert!(ok, "b-0-plain: {} {detail}", back.summary());
            vm.reboot("gpu-unlock VM test: case b starts without gpu_unlock");
        }
        vm.modprobe_files(case, None);
        // Each case generates the firmware afresh.
        vm.ok(&format!("rm -rf '{}/manager/gpu-unlock/firmware'", vm.data));
        unlock_row = vm.item_1_2(case);
        vm.item_3(case);
        vm.item_4(case);
    }

    let sizes = format!(
        "without the unlock {} bytes, with it {} bytes\n",
        vm.image_facts(&plain_row)["size"],
        vm.image_facts(&unlock_row)["size"]
    );
    vm.save("image-sizes.txt", &sizes);
    vm.note(&format!("image sizes: {sizes}"));

    vm.item_5(&unlock_row, &plain_row);
    vm.modprobe_files(Case::B, None);
    vm.finish();
}
