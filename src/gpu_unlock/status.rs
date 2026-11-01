//! The home page reading of the tensor unlock.
//!
//! Shown when the merged `/usr` carries the unlock rule. For each target card the kernel log of
//! this boot says what the module did to it. The reading is for the person at the page and
//! feeds no check or decision.

use std::path::Path;

use serde::Serialize;

use crate::codes::*;
use crate::facts::{self, Reading};
use crate::gpu_unlock::targets_ok;
use crate::host::{At, Cmd, Exit, Host};

const RULE_PATH: &str = "/usr/lib/modprobe.d/gpu-unlock.conf";
const TARGETS_PARAMETER: &str = "/sys/module/gpu_unlock/parameters/targets";
/// The kernel log lines the reading needs: the module's, and the two the driver prints about
/// a card without the module's prefix (`unlock_610.c:966`, `:1661`).
const JOURNAL_PATTERN: &str = "gpu_unlock|power off first|firmware did not stop";

/// What the module did to one card in this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardResult {
    /// The card was initialised or unlocked before and needs a power cycle.
    NotCold,
    Unlocked,
    /// The module is loaded with no targets; the card is unlocked at the next boot.
    NextBoot,
    NotUnlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub address: String,
    pub result: CardResult,
    /// The last log line about the card, for a card that was not unlocked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
}

/// The reading: one entry per target of the rule, and when this boot started (seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub cards: Vec<Card>,
    pub boot_time: Option<u64>,
}

/// The targets of the `options gpu_unlock targets=…` line, `None` when it is missing or holds
/// anything but distinct addresses.
fn parse_targets(rule: &str) -> Option<Vec<String>> {
    let line = rule
        .lines()
        .find_map(|l| l.trim().strip_prefix("options gpu_unlock "))?;
    let value = line
        .split_whitespace()
        .find_map(|token| token.strip_prefix("targets="))?;
    let targets: Vec<String> = value.split(',').map(str::to_string).collect();
    targets_ok(&targets).then_some(targets)
}

/// The log lines the table is read from: those of the module, and the two messages the
/// driver prints about a target without the module's prefix.
fn relevant_lines<'a>(journal: &'a str, targets: &[String]) -> Vec<&'a str> {
    let about_target = |line: &str| {
        targets.iter().any(|t| line.contains(t.as_str()))
            && (line.contains("power off first") || line.contains("firmware did not stop"))
    };
    journal
        .lines()
        .filter(|line| line.contains("gpu_unlock") || about_target(line))
        .collect()
}

/// The result of each target, the first row of the table that holds. `parameter` is the
/// module's `targets` parameter, `None` when the module is not loaded. A card has several lines
/// per unlock; its outcome is the last line that says `unlocked` or `failed`.
fn judge(targets: &[String], lines: &[&str], parameter: Option<&str>) -> Vec<Card> {
    let waiting = parameter.is_some_and(|p| matches!(p.trim(), "" | "(null)"));
    targets
        .iter()
        .map(|address| {
            let about: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| l.contains(address.as_str()))
                .collect();
            let outcome = about
                .iter()
                .rev()
                .find(|l| l.contains("unlocked") || l.contains("failed"));
            let not_cold = about.iter().any(|l| {
                [
                    "power off first",
                    "firmware did not stop",
                    "unlock failed (-114)",
                ]
                .iter()
                .any(|t| l.contains(t))
            });
            let result = if not_cold {
                CardResult::NotCold
            } else if outcome.is_some_and(|l| l.contains("unlocked")) {
                CardResult::Unlocked
            } else if waiting {
                CardResult::NextBoot
            } else {
                CardResult::NotUnlocked
            };
            let line = (result == CardResult::NotUnlocked)
                .then(|| outcome.or(about.last()).map(|l| l.to_string()))
                .flatten();
            Card {
                address: address.clone(),
                result,
                line,
            }
        })
        .collect()
}

/// `btime` of `/proc/stat`: when the system booted, in seconds.
fn parse_btime(stat: &str) -> Option<u64> {
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// Read the unlock status of this host: `Value(None)` when no rule is merged.
pub fn read(h: &dyn Host) -> Reading<Option<Status>> {
    let rule = match h.read(&h.view(Path::new(RULE_PATH))) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Reading::Value(None),
        Err(e) => return Reading::Unknown(crate::log::io_reason_key(&e)),
    };
    let Some(targets) = parse_targets(&rule) else {
        return Reading::Unknown(REASON_FORMAT_UNRECOGNIZED);
    };
    // journalctl keeps the matching lines and the last of them, so the reading never holds the
    // whole kernel log of the boot. When nothing matches it exits 1; `-q` keeps it from printing
    // `-- No entries --` in any output mode, so that case has no output at all.
    let journal = Cmd::new(At::Host, "journalctl", HOST_QUERY_TIMEOUT)
        .args(["-k", "-b", "-o", "cat", "--no-pager", "-q", "-n"])
        .arg(ERROR_RAW_LINES.to_string())
        .args(["--grep", JOURNAL_PATTERN]);
    let out = h.run(&journal);
    let none = out.exit == Exit::Code(1) && out.stdout.is_empty() && out.stderr.is_empty();
    if !out.exit.ok() && !none {
        return Reading::Unknown(facts::command_unknown_reason(out.exit));
    }
    let journal = out.stdout_text();
    let lines = relevant_lines(&journal, &targets);
    // An absent parameter means the module is not loaded; any other error leaves it unknown.
    let parameter = match h.read(&h.view(Path::new(TARGETS_PARAMETER))) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Reading::Unknown(crate::log::io_reason_key(&e)),
    };
    let boot_time = h
        .read(Path::new("/proc/stat"))
        .ok()
        .and_then(|bytes| parse_btime(&String::from_utf8_lossy(&bytes)));
    Reading::Value(Some(Status {
        cards: judge(&targets, &lines, parameter.as_deref()),
        boot_time,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0000:01:00.0";
    const B: &str = "0000:02:00.0";

    fn targets() -> Vec<String> {
        vec![A.to_string(), B.to_string()]
    }

    fn results(lines: &[&str], parameter: Option<&str>) -> Vec<(CardResult, Option<String>)> {
        judge(&targets(), lines, parameter)
            .into_iter()
            .map(|c| (c.result, c.line))
            .collect()
    }

    #[test]
    fn the_rule_gives_its_targets() {
        let rule = "# Written\nsoftdep nvidia pre: vfio-pci gpu_unlock\noptions gpu_unlock targets=0000:01:00.0,0000:02:00.0\n";
        assert_eq!(parse_targets(rule), Some(targets()));
        for bad in [
            "",
            "softdep nvidia pre: vfio-pci gpu_unlock\n",
            "options gpu_unlock\n",
            "options gpu_unlock targets=\n",
            "options gpu_unlock targets=all\n",
            "options gpu_unlock targets=0000:01:00.0,0000:01:00.0\n",
        ] {
            assert_eq!(parse_targets(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_unlocked_card_is_unlocked() {
        let lines = [
            format!("gpu_unlock: unlocked {A}"),
            format!("gpu_unlock: unlocked {B}"),
        ];
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let got = results(&lines, Some(&format!("{A},{B}")));
        assert_eq!(got, vec![(CardResult::Unlocked, None); 2]);
    }

    #[test]
    fn the_two_driver_messages_mean_the_card_needs_a_power_cycle() {
        let first = format!("NVRM: {A}: power off first");
        let second = format!("nvidia {B}: firmware did not stop");
        let got = results(&[&first, &second], Some(&format!("{A},{B}")));
        assert_eq!(got, vec![(CardResult::NotCold, None); 2]);
        let failed = format!("gpu_unlock {A}: unlock failed (-114)");
        assert_eq!(results(&[&failed], None)[0].0, CardResult::NotCold);
    }

    #[test]
    fn a_power_cycle_outranks_an_unlock_in_the_same_boot() {
        let unlocked = format!("gpu_unlock: unlocked {A}");
        let later = format!("nvidia {A}: firmware did not stop");
        assert_eq!(
            results(&[&unlocked, &later], None)[0].0,
            CardResult::NotCold
        );
    }

    #[test]
    fn messages_about_another_address_do_not_count() {
        let other = "NVRM: 0000:09:00.0: power off first".to_string();
        assert_eq!(results(&[&other], None)[0].0, CardResult::NotUnlocked);
    }

    #[test]
    fn a_loaded_module_without_targets_waits_for_the_next_boot() {
        for parameter in ["(null)\n", "", "\n"] {
            assert_eq!(
                results(&[], Some(parameter)),
                vec![(CardResult::NextBoot, None); 2],
                "{parameter:?}"
            );
        }
        // Set targets, or no module in the kernel, are not "next boot".
        assert_eq!(results(&[], Some(A))[0].0, CardResult::NotUnlocked);
        assert_eq!(results(&[], None)[0].0, CardResult::NotUnlocked);
    }

    #[test]
    fn a_card_that_was_not_unlocked_carries_its_last_line() {
        let first = format!("gpu_unlock {A}: unlock failed (-16)");
        let last = format!("gpu_unlock {A}: firmware load failed (-2)");
        let got = results(&[&first, &last], Some(A));
        assert_eq!(got[0], (CardResult::NotUnlocked, Some(last)));
        assert_eq!(got[1], (CardResult::NotUnlocked, None));
    }

    #[test]
    fn the_kept_lines_are_the_modules_and_the_two_messages_of_the_targets() {
        let journal = format!(
            "boot\ngpu_unlock: loaded\nNVRM: {A}: power off first\nNVRM: 0000:09:00.0: power off first\nNVRM: {B}: ok\nnvidia {B}: firmware did not stop\n"
        );
        let kept = relevant_lines(&journal, &targets());
        assert_eq!(
            kept,
            vec![
                "gpu_unlock: loaded",
                &format!("NVRM: {A}: power off first"),
                &format!("nvidia {B}: firmware did not stop"),
            ]
        );
    }

    #[test]
    fn a_real_boot_with_no_supported_card_is_not_unlocked_and_shows_the_line() {
        // The kernel log after `modprobe -a nvidia nvidia_drm nvidia_modeset` on a fresh boot.
        let address = "0000:00:1f.7".to_string();
        let line = r#"gpu_unlock: no supported card at "0000:00:1f.7""#;
        let journal = format!("{line}\nNVRM: No NVIDIA GPU found.\n").repeat(3);
        let lines = relevant_lines(&journal, std::slice::from_ref(&address));
        assert_eq!(lines, vec![line; 3]);
        let cards = judge(&[address], &lines, Some("0000:00:1f.7\n"));
        assert_eq!(cards[0].result, CardResult::NotUnlocked);
        assert_eq!(cards[0].line.as_deref(), Some(line));
    }

    #[test]
    fn the_last_outcome_of_a_card_decides() {
        let boot = format!("gpu_unlock: unlocked {A}");
        let failed = format!("gpu_unlock {A}: unlock after resume failed (-5)");
        let got = results(&[&boot, &failed], Some(A));
        assert_eq!(got[0], (CardResult::NotUnlocked, Some(failed.clone())));
        let resumed = format!("gpu_unlock {A}: still unlocked after resume");
        assert_eq!(
            results(&[&resumed], Some(A))[0],
            (CardResult::Unlocked, None)
        );
        let again = format!("gpu_unlock {A}: unlocked again after resume");
        assert_eq!(
            results(&[&failed, &again], Some(A))[0].0,
            CardResult::Unlocked
        );
    }

    #[test]
    fn stage_lines_after_the_outcome_change_nothing() {
        let stages: Vec<String> = (0..300)
            .map(|i| format!("gpu_unlock: stage={i} {A}"))
            .collect();
        let unlocked = format!("gpu_unlock: unlocked {A}");
        let failed = format!("gpu_unlock {A}: unlock failed (-16)");
        let mut lines: Vec<&str> = vec![&unlocked];
        lines.extend(stages.iter().map(String::as_str));
        assert_eq!(results(&lines, Some(A))[0], (CardResult::Unlocked, None));
        let mut lines: Vec<&str> = vec![&failed];
        lines.extend(stages.iter().map(String::as_str));
        assert_eq!(
            results(&lines, Some(A))[0],
            (CardResult::NotUnlocked, Some(failed.clone()))
        );
        // Without any outcome line the last line about the card is shown.
        assert_eq!(
            results(&lines[1..], Some(A))[0].1.as_deref(),
            Some(stages[299].as_str())
        );
    }

    #[test]
    fn the_boot_time_is_the_btime_line() {
        assert_eq!(
            parse_btime("cpu 1 2\nbtime 1790000000\nprocesses 5\n"),
            Some(1_790_000_000)
        );
        assert_eq!(parse_btime("cpu 1 2\n"), None);
        assert_eq!(parse_btime("btime soon\n"), None);
    }
}
