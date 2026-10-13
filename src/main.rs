use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table};
use crossterm::terminal;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn get_home_dir() -> Result<PathBuf> {
    if let Some(orig) = env::var_os("AGX_ORIGINAL_HOME") {
        return Ok(PathBuf::from(orig));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("Could not determine user HOME directory from environment")?;
    if !home.join(".agx").exists() {
        if let Some(parent) = home.parent() {
            if parent.file_name().and_then(|s| s.to_str()) == Some(".agx") {
                if let Some(grandparent) = parent.parent() {
                    return Ok(grandparent.to_path_buf());
                }
            }
        }
    }
    Ok(home)
}

fn get_agx_root() -> Result<PathBuf> {
    let home = get_home_dir()?;
    Ok(home.join(".agx"))
}

fn get_instance_dir(instance_name: &str) -> Result<PathBuf> {
    let agx_root = get_agx_root()?;
    Ok(agx_root.join(instance_name))
}

fn get_all_instances() -> Result<Vec<String>> {
    let agx_root = get_agx_root()?;
    if !agx_root.exists() {
        return Ok(Vec::new());
    }

    let mut instances = Vec::new();
    for entry in fs::read_dir(&agx_root).context("Failed to read ~/.agx directory")? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with('.') {
                    instances.push(name.to_string());
                }
            }
        }
    }
    instances.sort();
    Ok(instances)
}

fn get_keychain_link_targets(home: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut targets = Vec::new();
    if cfg!(target_os = "macos") {
        targets.push(("macOS Keychain", home.join("Library").join("Keychains")));
    } else if cfg!(target_os = "linux") {
        // GNOME Keyring and Freedesktop Secret Service standard storage
        targets.push(("GNOME Keyring", home.join(".local").join("share").join("keyrings")));
        // KDE KWallet storage
        targets.push(("KDE KWallet data", home.join(".local").join("share").join("kwalletd")));
        // KDE KWallet configuration
        targets.push(("KDE KWallet config", home.join(".config").join("kwalletrc")));
    }
    targets
}

fn link_single_keychain_entry(label: &str, source: &Path, target: &Path) -> Result<bool> {
    if !source.exists() && !source.is_symlink() {
        return Ok(false);
    }

    if target.is_symlink() {
        if let Ok(dest) = fs::read_link(target) {
            if dest == source {
                return Ok(false); // Already correctly linked
            }
        }
        let _ = fs::remove_file(target);
    } else if target.is_dir() {
        // If target is an existing directory, we shouldn't overwrite it blindly
        // unless it's empty, or warn user.
        return Ok(false);
    } else if target.exists() {
        return Ok(false);
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).with_context(|| {
            format!(
                "Failed to link {} from {} to {}",
                label,
                source.display(),
                target.display()
            )
        })?;
    }

    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(source, target)?;
        } else {
            std::os::windows::fs::symlink_file(source, target)?;
        }
    }

    Ok(true)
}

fn link_instance_keychain(instance_name: &str, verbose: bool) -> Result<usize> {
    let home = get_home_dir()?;
    let instance_dir = get_instance_dir(instance_name)?;
    let mut linked_count = 0;

    let targets = get_keychain_link_targets(&home);
    for (label, source) in targets {
        // Compute relative path from home to source
        if let Ok(rel) = source.strip_prefix(&home) {
            let target = instance_dir.join(rel);
            match link_single_keychain_entry(label, &source, &target) {
                Ok(true) => {
                    linked_count += 1;
                    if verbose {
                        println!(
                            "  [+] Linked {} ({} -> {})",
                            label,
                            target.display(),
                            source.display()
                        );
                    }
                }
                Ok(false) => {
                    // Either source didn't exist or already linked
                }
                Err(err) => {
                    if verbose {
                        eprintln!("  [!] Warning linking {}: {:#}", label, err);
                    }
                }
            }
        }
    }

    Ok(linked_count)
}

fn handle_keychain(target_instance: Option<&str>) -> Result<()> {
    let instances = if let Some(inst) = target_instance {
        let instance_dir = get_instance_dir(inst)?;
        if !instance_dir.exists() {
            bail!("Instance '{}' does not exist at {}", inst, instance_dir.display());
        }
        vec![inst.to_string()]
    } else {
        let all = get_all_instances()?;
        if all.is_empty() {
            println!("No instances found.");
            return Ok(());
        }
        all
    };

    println!("Ensuring keychain symlinks for {} instance(s)...", instances.len());
    let mut total_linked = 0;
    for inst in &instances {
        println!("Checking instance '{}'...", inst);
        let count = link_instance_keychain(inst, true)?;
        total_linked += count;
    }

    println!("Done. Created/updated {} keychain link(s).", total_linked);
    Ok(())
}

fn handle_add(instance_name: &str) -> Result<()> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }
    let instance_dir = get_instance_dir(instance_name)?;
    if instance_dir.exists() {
        println!("Instance '{}' already exists at {}", instance_name, instance_dir.display());
    } else {
        fs::create_dir_all(&instance_dir)
            .with_context(|| format!("Failed to create directory {}", instance_dir.display()))?;
        println!("Created instance '{}' at {}", instance_name, instance_dir.display());
    }

    let _ = link_instance_keychain(instance_name, false);
    Ok(())
}

fn is_instance_disabled(instance_name: &str) -> Result<bool> {
    let instance_dir = get_instance_dir(instance_name)?;
    Ok(instance_dir.join(".disabled").exists())
}

fn handle_enable(instance_name: &str) -> Result<()> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }
    let instance_dir = get_instance_dir(instance_name)?;
    if !instance_dir.exists() {
        bail!("Instance '{}' does not exist at {}", instance_name, instance_dir.display());
    }

    let disabled_file = instance_dir.join(".disabled");
    if disabled_file.exists() {
        fs::remove_file(&disabled_file)
            .with_context(|| format!("Failed to enable instance '{}'", instance_name))?;
        println!("Enabled instance '{}'", instance_name);
    } else {
        println!("Instance '{}' is already enabled", instance_name);
    }
    Ok(())
}

fn handle_disable(instance_name: &str) -> Result<()> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }
    let instance_dir = get_instance_dir(instance_name)?;
    if !instance_dir.exists() {
        bail!("Instance '{}' does not exist at {}", instance_name, instance_dir.display());
    }

    let disabled_file = instance_dir.join(".disabled");
    if !disabled_file.exists() {
        fs::write(&disabled_file, "")
            .with_context(|| format!("Failed to disable instance '{}'", instance_name))?;
        println!("Disabled instance '{}'", instance_name);
    } else {
        println!("Instance '{}' is already disabled", instance_name);
    }
    Ok(())
}

fn handle_list() -> Result<()> {
    let instances = get_all_instances()?;
    let agx_root = get_agx_root()?;

    if instances.is_empty() {
        println!("No instances found in {}", agx_root.display());
    } else {
        println!("Available instances:");
        for inst in instances {
            let disabled = is_instance_disabled(&inst).unwrap_or(false);
            if disabled {
                println!("  - {} (disabled)", inst);
            } else {
                println!("  - {}", inst);
            }
        }
    }
    Ok(())
}

use serde::Deserialize;

#[derive(Debug, Clone)]
struct QuotaMetric {
    model_family: String,
    limit_type: String,
    remaining_pct: u32,
    reset_time: String,
    human_reset: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AgyUsageResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    command: Option<AgyCommand>,
}

#[derive(Debug, Deserialize)]
struct AgyCommand {
    #[serde(default)]
    data: Option<AgyUsageData>,
}

#[derive(Debug, Deserialize)]
struct AgyUsageData {
    #[serde(default)]
    groups: Vec<AgyQuotaGroup>,
}

#[derive(Debug, Deserialize)]
struct AgyQuotaGroup {
    name: String,
    #[serde(default)]
    buckets: Vec<AgyQuotaBucket>,
}

#[derive(Debug, Deserialize)]
struct AgyQuotaBucket {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
}

#[derive(Debug)]
struct InstanceUsage {
    instance_name: String,
    metrics: Vec<QuotaMetric>,
    error: Option<String>,
}

fn render_progress_bar(pct: u32, width: usize) -> String {
    let clamped_pct = pct.min(100);
    let filled = (clamped_pct as usize * width) / 100;
    let empty = width.saturating_sub(filled);
    format!("[{}{}] {:>3}%", "█".repeat(filled), "░".repeat(empty), clamped_pct)
}

fn progress_color(pct: u32) -> Color {
    if pct >= 50 {
        Color::Green
    } else if pct >= 20 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Parses ISO-8601 (e.g. "2026-10-18T07:32:48Z") to unix timestamp
fn parse_iso8601_to_timestamp(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let date_parts: Vec<&str> = s[0..10].split('-').collect();
    let time_parts: Vec<&str> = s[11..19].split(':').collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }

    let year = date_parts[0].parse::<i64>().ok()?;
    let month = date_parts[1].parse::<i64>().ok()?;
    let day = date_parts[2].parse::<i64>().ok()?;
    let hour = time_parts[0].parse::<i64>().ok()?;
    let min = time_parts[1].parse::<i64>().ok()?;
    let sec = time_parts[2].parse::<i64>().ok()?;

    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }

    // Days from civil date (algorithm by Howard Hinnant)
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * m + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146097) as i64 + doe as i64 - 719468;

    let timestamp = days * 86400 + hour * 3600 + min * 60 + sec;
    if timestamp >= 0 {
        Some(timestamp as u64)
    } else {
        None
    }
}

/// Formats duration seconds into compact human relative time e.g. "in 2d 3h 5min 12s", "in 4h 30min 5s", "in 5min 20s", "in 45s"
fn format_relative_seconds(secs: u64) -> String {
    if secs == 0 {
        return "now".to_string();
    }
    let days = secs / 86400;
    let rem = secs % 86400;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}min", minutes));
    }
    if seconds > 0 || (days == 0 && hours == 0 && minutes == 0) {
        parts.push(format!("{}s", seconds));
    }

    format!("in {}", parts.join(" "))
}

/// Computes compact human relative time for reset (including seconds)
fn format_human_relative_time(reset_time_str: &str, bucket_desc: Option<&str>) -> Option<String> {
    // 1. First prioritize parsing ISO timestamp and comparing with current system time for second-level accuracy
    if let Some(target_ts) = parse_iso8601_to_timestamp(reset_time_str) {
        if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            let now_ts = now.as_secs();
            if target_ts > now_ts {
                let diff = target_ts - now_ts;
                return Some(format_relative_seconds(diff));
            } else {
                return Some("passed".to_string());
            }
        }
    }

    // 2. Fallback: Parse from bucket description if available (e.g. "refresh in 5 days, 1 hour")
    if let Some(desc) = bucket_desc {
        let lower = desc.to_lowercase();
        if let Some(pos) = lower.find("refresh in ") {
            let text = &desc[pos + "refresh in ".len()..];
            let mut days = 0u64;
            let mut hours = 0u64;
            let mut minutes = 0u64;
            let mut seconds = 0u64;

            for segment in text.split(',') {
                let s = segment.trim().trim_end_matches('.');
                let tokens: Vec<&str> = s.split_whitespace().collect();
                if tokens.len() == 2 {
                    if let Ok(val) = tokens[0].parse::<u64>() {
                        if tokens[1].starts_with("day") {
                            days = val;
                        } else if tokens[1].starts_with("hour") {
                            hours = val;
                        } else if tokens[1].starts_with("min") {
                            minutes = val;
                        } else if tokens[1].starts_with("sec") {
                            seconds = val;
                        }
                    }
                }
            }

            let mut parts = Vec::new();
            if days > 0 {
                parts.push(format!("{}d", days));
            }
            if hours > 0 {
                parts.push(format!("{}h", hours));
            }
            if minutes > 0 {
                parts.push(format!("{}min", minutes));
            }
            if seconds > 0 || (days == 0 && hours == 0 && minutes == 0) {
                parts.push(format!("{}s", seconds));
            }
            if !parts.is_empty() {
                return Some(format!("in {}", parts.join(" ")));
            }
        }
    }

    None
}

fn parse_usage_output(output_str: &str) -> Vec<QuotaMetric> {
    // 1. First attempt to parse as JSON
    if let Ok(json_val) = serde_json::from_str::<AgyUsageResponse>(output_str.trim()) {
        if let Some(cmd) = json_val.command {
            if let Some(data) = cmd.data {
                let mut metrics = Vec::new();
                for group in data.groups {
                    for bucket in group.buckets {
                        let pct = match bucket.remaining_fraction {
                            Some(frac) => (frac * 100.0).round().clamp(0.0, 100.0) as u32,
                            None => 0,
                        };
                        let reset_time = bucket.reset_time.clone().unwrap_or_else(|| "-".to_string());
                        let human_reset = format_human_relative_time(&reset_time, bucket.description.as_deref());
                        metrics.push(QuotaMetric {
                            model_family: group.name.clone(),
                            limit_type: bucket.name,
                            remaining_pct: pct,
                            reset_time,
                            human_reset,
                        });
                    }
                }
                if !metrics.is_empty() {
                    return metrics;
                }
            }
        }

        // Check if json has text response inside
        if let Some(resp_text) = json_val.response {
            let metrics = parse_text_usage(&resp_text);
            if !metrics.is_empty() {
                return metrics;
            }
        }
    }

    // 2. Fallback to direct text parsing
    parse_text_usage(output_str)
}

fn parse_text_usage(output_str: &str) -> Vec<QuotaMetric> {
    let mut metrics = Vec::new();
    for line in output_str.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("quota:") {
            continue;
        }

        // Try tab-delimited
        let tab_parts: Vec<&str> = line.split('\t').map(str::trim).filter(|s| !s.is_empty()).collect();
        if tab_parts.len() >= 3 {
            let model_family = tab_parts[0].to_string();
            let limit_type = tab_parts[1].to_string();
            let pct_str = tab_parts[2].trim_end_matches('%');
            if let Ok(pct) = pct_str.parse::<u32>() {
                let reset_time = if tab_parts.len() >= 4 {
                    tab_parts[3].to_string()
                } else {
                    "-".to_string()
                };
                let human_reset = format_human_relative_time(&reset_time, None);
                metrics.push(QuotaMetric {
                    model_family,
                    limit_type,
                    remaining_pct: pct,
                    reset_time,
                    human_reset,
                });
                continue;
            }
        }

        // Try whitespace parsing with known limits
        let known_limits = ["weekly limit remaining", "five hour limit remaining"];
        let lower = line.to_lowercase();
        for &limit in &known_limits {
            if let Some(idx) = lower.find(limit) {
                let model_family = line[..idx].trim().to_string();
                let limit_type = line[idx..idx + limit.len()].trim().to_string();
                let rest = line[idx + limit.len()..].trim();
                let rest_parts: Vec<&str> = rest.split_whitespace().collect();
                if !rest_parts.is_empty() {
                    let pct_str = rest_parts[0].trim_end_matches('%');
                    if let Ok(pct) = pct_str.parse::<u32>() {
                        let reset_time = if rest_parts.len() > 1 {
                            rest_parts[1..].join(" ")
                        } else {
                            "-".to_string()
                        };
                        let human_reset = format_human_relative_time(&reset_time, None);
                        metrics.push(QuotaMetric {
                            model_family,
                            limit_type,
                            remaining_pct: pct,
                            reset_time,
                            human_reset,
                        });
                        break;
                    }
                }
            }
        }
    }
    metrics
}

fn query_instance_usage(instance_name: &str) -> InstanceUsage {
    if is_instance_disabled(instance_name).unwrap_or(false) {
        return InstanceUsage {
            instance_name: instance_name.to_string(),
            metrics: Vec::new(),
            error: Some("Disabled".to_string()),
        };
    }

    let instance_dir = match get_instance_dir(instance_name) {
        Ok(dir) => dir,
        Err(err) => {
            return InstanceUsage {
                instance_name: instance_name.to_string(),
                metrics: Vec::new(),
                error: Some(format!("Dir error: {:#}", err)),
            }
        }
    };

    let output_result = Command::new("agy")
        .arg("-p")
        .arg("/usage")
        .arg("--disable-slash-commands=false")
        .arg("--output-format")
        .arg("json")
        .env("HOME", &instance_dir)
        .output();

    match output_result {
        Ok(output) => {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}\n{}", stdout_str, stderr_str);
            let metrics = parse_usage_output(&combined);
            if metrics.is_empty() {
                let err_msg = if !output.status.success() {
                    format!("Exit code {}: {}", output.status, stderr_str.trim())
                } else if combined.trim().is_empty() {
                    "No output returned by agy".to_string()
                } else {
                    format!("Could not parse output: {}", combined.trim().lines().next().unwrap_or(""))
                };
                InstanceUsage {
                    instance_name: instance_name.to_string(),
                    metrics: Vec::new(),
                    error: Some(err_msg),
                }
            } else {
                InstanceUsage {
                    instance_name: instance_name.to_string(),
                    metrics,
                    error: None,
                }
            }
        }
        Err(err) => InstanceUsage {
            instance_name: instance_name.to_string(),
            metrics: Vec::new(),
            error: Some(format!("Failed to run agy: {:#}", err)),
        },
    }
}

fn render_usage_tables(results: &[InstanceUsage], show_summary: bool) {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Instance").fg(Color::Cyan),
            Cell::new("Model Family").fg(Color::Cyan),
            Cell::new("Quota / Limit").fg(Color::Cyan),
            Cell::new("Remaining").fg(Color::Cyan),
            Cell::new("Progress").fg(Color::Cyan),
            Cell::new("Resets At").fg(Color::Cyan),
        ]);

    let mut aggregated: BTreeMap<(String, String), (u64, u32)> = BTreeMap::new(); // (Model, Limit) -> (sum_pct, count)

    for res in results {
        if let Some(err) = &res.error {
            let (status_col, desc_col) = if err == "Disabled" {
                (Cell::new("Disabled").fg(Color::DarkGrey), Cell::new("-").set_alignment(CellAlignment::Center))
            } else {
                (Cell::new("Error / Offline").fg(Color::Red), Cell::new(err).fg(Color::DarkGrey))
            };

            table.add_row(vec![
                Cell::new(&res.instance_name).fg(Color::White),
                status_col,
                desc_col,
                Cell::new("-").set_alignment(CellAlignment::Center),
                Cell::new("-").set_alignment(CellAlignment::Center),
                Cell::new("-").set_alignment(CellAlignment::Center),
            ]);
            continue;
        }

        for (i, m) in res.metrics.iter().enumerate() {
            let key = (m.model_family.clone(), m.limit_type.clone());
            let entry = aggregated.entry(key).or_insert((0, 0));
            entry.0 += m.remaining_pct as u64;
            entry.1 += 1;

            let inst_col = if i == 0 {
                Cell::new(&res.instance_name).fg(Color::White)
            } else {
                Cell::new("")
            };

            let color = progress_color(m.remaining_pct);
            let bar = render_progress_bar(m.remaining_pct, 14);

            let reset_display = match &m.human_reset {
                Some(human) => human.clone(),
                None => m.reset_time.clone(),
            };

            table.add_row(vec![
                inst_col,
                Cell::new(&m.model_family).fg(Color::Yellow),
                Cell::new(&m.limit_type),
                Cell::new(format!("{}%", m.remaining_pct))
                    .fg(color)
                    .set_alignment(CellAlignment::Right),
                Cell::new(bar).fg(color),
                Cell::new(reset_display).fg(Color::DarkGrey),
            ]);
        }
    }

    println!("\n{}", table);

    // Render aggregated summary table
    if show_summary && !aggregated.is_empty() {
        let mut agg_table = Table::new();
        agg_table
            .load_style(UTF8_FULL.with_rounded_corners())
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Total Aggregated (Average)").fg(Color::Magenta),
                Cell::new("Quota / Limit").fg(Color::Magenta),
                Cell::new("Instances").fg(Color::Magenta),
                Cell::new("Avg Remaining").fg(Color::Magenta),
                Cell::new("Progress").fg(Color::Magenta),
            ]);

        for ((model, limit), (sum_pct, count)) in &aggregated {
            let avg_pct = (*sum_pct / *count as u64) as u32;
            let color = progress_color(avg_pct);
            let bar = render_progress_bar(avg_pct, 16);

            agg_table.add_row(vec![
                Cell::new(model).fg(Color::Yellow),
                Cell::new(limit),
                Cell::new(format!("{}", count)).set_alignment(CellAlignment::Center),
                Cell::new(format!("{}%", avg_pct))
                    .fg(color)
                    .set_alignment(CellAlignment::Right),
                Cell::new(bar).fg(color),
            ]);
        }

        println!("\n{}", agg_table);
    }
}

fn handle_usage() -> Result<()> {
    let instances = get_all_instances()?;
    if instances.is_empty() {
        println!("No instances found in ~/.agx. Run 'agx add <name>' first.");
        return Ok(());
    }

    println!("Fetching usage for {} instance(s)...", instances.len());

    // Query in parallel
    let mut handles = Vec::new();
    for inst in instances {
        handles.push(thread::spawn(move || query_instance_usage(&inst)));
    }

    let mut results = Vec::new();
    for handle in handles {
        if let Ok(res) = handle.join() {
            results.push(res);
        }
    }
    results.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));

    render_usage_tables(&results, results.len() > 1);
    Ok(())
}

fn handle_instance_usage(instance_name: &str) -> Result<()> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }
    let instance_dir = get_instance_dir(instance_name)?;
    if !instance_dir.exists() {
        bail!("Instance '{}' does not exist at {}", instance_name, instance_dir.display());
    }

    println!("Fetching usage for instance '{}'...", instance_name);
    let res = query_instance_usage(instance_name);
    render_usage_tables(&[res], false);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HealthStatus {
    Healthy,
    Unhealthy,
    Disabled,
}

#[derive(Debug, Clone)]
struct PingResult {
    instance_name: String,
    status: HealthStatus,
    latency: Option<std::time::Duration>,
    message: String,
}

fn ping_instance(instance_name: &str, check_disabled: bool) -> PingResult {
    if check_disabled && is_instance_disabled(instance_name).unwrap_or(false) {
        return PingResult {
            instance_name: instance_name.to_string(),
            status: HealthStatus::Disabled,
            latency: None,
            message: "-".to_string(),
        };
    }

    let instance_dir = match get_instance_dir(instance_name) {
        Ok(dir) => dir,
        Err(err) => {
            return PingResult {
                instance_name: instance_name.to_string(),
                status: HealthStatus::Unhealthy,
                latency: None,
                message: format!("Dir error: {:#}", err),
            };
        }
    };

    if !instance_dir.exists() {
        return PingResult {
            instance_name: instance_name.to_string(),
            status: HealthStatus::Unhealthy,
            latency: None,
            message: "Instance directory does not exist".to_string(),
        };
    }

    let start = std::time::Instant::now();
    let child_res = Command::new("agy")
        .arg("--prompt")
        .arg("whats between 1 and 3")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("HOME", &instance_dir)
        .spawn();

    let mut child = match child_res {
        Ok(c) => c,
        Err(err) => {
            return PingResult {
                instance_name: instance_name.to_string(),
                status: HealthStatus::Unhealthy,
                latency: None,
                message: format!("Failed to spawn agy: {:#}", err),
            };
        }
    };

    let timeout = std::time::Duration::from_secs(60);
    let poll_interval = std::time::Duration::from_millis(100);
    let mut waited = std::time::Duration::ZERO;

    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if waited >= timeout {
                    let _ = child.kill();
                    break None;
                }
                thread::sleep(poll_interval);
                waited += poll_interval;
            }
            Err(_) => {
                let _ = child.kill();
                break None;
            }
        }
    };

    let duration = start.elapsed();

    let mut stdout_str = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout_str);
    }
    let mut stderr_str = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr_str);
    }

    let exit_status = match exit_status {
        Some(s) => s,
        None => {
            return PingResult {
                instance_name: instance_name.to_string(),
                status: HealthStatus::Unhealthy,
                latency: Some(duration),
                message: "Timed out waiting for response (60s)".to_string(),
            };
        }
    };

    let combined = format!("{}\n{}", stdout_str, stderr_str);
    let trimmed_stdout = stdout_str.trim();

    let is_auth_error = combined.contains("Authentication required")
        || combined.contains("Waiting for authentication")
        || combined.contains("Unauthorized")
        || combined.contains("Please visit the URL to log in");

    if !exit_status.success() || is_auth_error {
        let mut err_msg = if is_auth_error {
            "Authentication required".to_string()
        } else {
            let err_first_line = stderr_str
                .lines()
                .chain(stdout_str.lines())
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("Process failed without output");
            format!("Exit code {}: {}", exit_status, err_first_line)
        };
        if err_msg.len() > 60 {
            err_msg = format!("{}...", &err_msg[..57]);
        }
        return PingResult {
            instance_name: instance_name.to_string(),
            status: HealthStatus::Unhealthy,
            latency: Some(duration),
            message: err_msg,
        };
    }

    if trimmed_stdout.is_empty() {
        return PingResult {
            instance_name: instance_name.to_string(),
            status: HealthStatus::Unhealthy,
            latency: Some(duration),
            message: "No output received from agy".to_string(),
        };
    }

    let response_preview = trimmed_stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(trimmed_stdout);

    let clean_preview = if response_preview.len() > 60 {
        format!("{}...", &response_preview[..57])
    } else {
        response_preview.to_string()
    };

    PingResult {
        instance_name: instance_name.to_string(),
        status: HealthStatus::Healthy,
        latency: Some(duration),
        message: clean_preview,
    }
}

fn render_ping_table(results: &[PingResult]) {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Instance").fg(Color::Cyan),
            Cell::new("Status").fg(Color::Cyan),
            Cell::new("Latency").fg(Color::Cyan),
            Cell::new("Response / Message").fg(Color::Cyan),
        ]);

    let mut healthy_count = 0;
    let mut unhealthy_count = 0;
    let mut disabled_count = 0;

    for res in results {
        let (status_cell, latency_cell, msg_cell) = match res.status {
            HealthStatus::Healthy => {
                healthy_count += 1;
                let lat_str = res
                    .latency
                    .map(|d| format!("{:.2}s", d.as_secs_f64()))
                    .unwrap_or_else(|| "-".to_string());
                (
                    Cell::new("● Healthy").fg(Color::Green),
                    Cell::new(lat_str).set_alignment(CellAlignment::Right).fg(Color::Green),
                    Cell::new(&res.message).fg(Color::White),
                )
            }
            HealthStatus::Unhealthy => {
                unhealthy_count += 1;
                let lat_str = res
                    .latency
                    .map(|d| format!("{:.2}s", d.as_secs_f64()))
                    .unwrap_or_else(|| "-".to_string());
                (
                    Cell::new("● Unhealthy").fg(Color::Red),
                    Cell::new(lat_str).set_alignment(CellAlignment::Right).fg(Color::Red),
                    Cell::new(&res.message).fg(Color::DarkGrey),
                )
            }
            HealthStatus::Disabled => {
                disabled_count += 1;
                (
                    Cell::new("● Disabled").fg(Color::DarkGrey),
                    Cell::new("-").set_alignment(CellAlignment::Center).fg(Color::DarkGrey),
                    Cell::new("-").set_alignment(CellAlignment::Center).fg(Color::DarkGrey),
                )
            }
        };

        table.add_row(vec![
            Cell::new(&res.instance_name).fg(Color::White),
            status_cell,
            latency_cell,
            msg_cell,
        ]);
    }

    println!("\n{}", table);

    if results.len() > 1 {
        println!(
            "\nSummary: {} healthy, {} unhealthy, {} disabled (Total: {})",
            healthy_count, unhealthy_count, disabled_count, results.len()
        );
    }
}

fn handle_ping_instance(instance_name: &str) -> Result<()> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }
    let instance_dir = get_instance_dir(instance_name)?;
    if !instance_dir.exists() {
        bail!("Instance '{}' does not exist at {}", instance_name, instance_dir.display());
    }

    let is_disabled = is_instance_disabled(instance_name).unwrap_or(false);
    if is_disabled {
        println!("Pinging instance '{}' (currently disabled)...", instance_name);
    } else {
        println!("Pinging instance '{}'...", instance_name);
    }

    let mut res = ping_instance(instance_name, false);
    if is_disabled {
        res.instance_name = format!("{} (disabled)", res.instance_name);
    }

    render_ping_table(&[res]);
    Ok(())
}

fn handle_ping_all() -> Result<()> {
    let instances = get_all_instances()?;
    if instances.is_empty() {
        let agx_root = get_agx_root()?;
        println!("No instances found in {}. Run 'agx add <name>' first.", agx_root.display());
        return Ok(());
    }

    println!("Pinging {} instance(s)...", instances.len());

    let mut handles = Vec::new();
    for inst in instances {
        handles.push(thread::spawn(move || ping_instance(&inst, true)));
    }

    let mut results = Vec::new();
    for handle in handles {
        if let Ok(res) = handle.join() {
            results.push(res);
        }
    }
    results.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));

    render_ping_table(&results);
    Ok(())
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn handle_run(instance_name: &str, raw_args: &[String]) -> Result<u32> {
    if instance_name.is_empty() || instance_name.contains('/') || instance_name.contains('\\') {
        bail!("Invalid instance name: '{}'", instance_name);
    }

    let instance_dir = get_instance_dir(instance_name)?;
    if is_instance_disabled(instance_name).unwrap_or(false) {
        bail!("Instance '{}' is disabled. Re-enable it with: agx {} enable", instance_name, instance_name);
    }

    if !instance_dir.exists() {
        fs::create_dir_all(&instance_dir)
            .with_context(|| format!("Failed to create instance directory {}", instance_dir.display()))?;
        let _ = link_instance_keychain(instance_name, false);
        eprintln!("[agx] Created new instance directory: {}", instance_dir.display());
    }

    // Map arguments: "-d" -> "--dangerously-skip-permissions"
    let mut mapped_args = Vec::new();
    for arg in raw_args {
        if arg == "-d" {
            mapped_args.push("--dangerously-skip-permissions".to_string());
        } else {
            mapped_args.push(arg.clone());
        }
    }

    // Determine initial terminal size
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Failed to open pseudoterminal (PTY)")?;

    // Build command for agy
    let mut cmd = CommandBuilder::new("agy");
    for arg in &mapped_args {
        cmd.arg(arg);
    }

    // Set working directory to the current working directory where agx was invoked
    if let Ok(current_dir) = env::current_dir() {
        cmd.cwd(current_dir);
    }

    // Set isolated HOME directory
    cmd.env("HOME", &instance_dir);
    if let Ok(home) = get_home_dir() {
        cmd.env("AGX_ORIGINAL_HOME", home);
    }

    // Spawn process in PTY slave
    let mut child = pair.slave.spawn_command(cmd).context("Failed to spawn 'agy'. Is 'agy' installed and available in PATH?")?;
    // Drop slave in parent so EOF is handled properly on slave close
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("Failed to clone PTY reader")?;
    let mut writer = pair.master.take_writer().context("Failed to take PTY writer")?;

    let is_running = Arc::new(AtomicBool::new(true));

    // Thread: PTY master output -> stdout
    let is_running_clone = Arc::clone(&is_running);
    let pty_to_stdout_handle = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        let mut stdout = io::stdout();
        while is_running_clone.load(Ordering::Relaxed) {
            match reader.read(&mut buffer) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if stdout.write_all(&buffer[..n]).is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Enter raw mode for stdin passthrough
    let _raw_mode = RawModeGuard::new().context("Failed to enable raw terminal mode")?;

    // Thread: stdin -> PTY master input
    let is_running_clone2 = Arc::clone(&is_running);
    let _stdin_to_pty_handle = thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0u8; 1024];
        while is_running_clone2.load(Ordering::Relaxed) {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if writer.write_all(&buffer[..n]).is_err() || writer.flush().is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Monitor resize events and wait for child process
    let mut last_size = (cols, rows);
    let exit_status = loop {
        // Check terminal resize
        if let Ok(current_size) = terminal::size() {
            if current_size != last_size {
                last_size = current_size;
                let _ = pair.master.resize(PtySize {
                    rows: current_size.1,
                    cols: current_size.0,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }

        thread::sleep(std::time::Duration::from_millis(50));
    };

    is_running.store(false, Ordering::Relaxed);
    drop(_raw_mode); // Explicitly restore terminal mode before exiting

    let _ = pty_to_stdout_handle.join();

    let exit_code = if exit_status.success() { 0 } else { 1 };

    Ok(exit_code)
}

fn print_help() {
    eprintln!(
        r#"agx - Multi-account manager & wrapper for agy

USAGE:
    agx <instance-name> [args...]    Run agy with HOME=~/.agx/<instance-name>
    agx <instance-name> usage        Display usage quotas for a specific instance
    agx <instance-name> enable       Enable a previously disabled instance
    agx <instance-name> disable      Disable an instance from runs & quota checks
    agx <instance-name> ping         Ping instance to check health
    agx <instance-name> keychain     Link/fix system keychain for a specific instance
    agx keychain [instance-name]     Link/fix system keychain for all (or specific) instances
    agx usage [instance-name]        Display usage quotas for all (or specific) instances
    agx ping [instance-name]         Ping all (or specific) instances to check health
    agx add <instance-name>          Create a new instance environment
    agx list                         List all configured instances
    agx --help, -h                   Show this help message

OPTIONS FOR RUN:
    -d                               Passes '--dangerously-skip-permissions' to agy
    [args...]                        All other arguments are forwarded directly to agy

EXAMPLES:
    agx keychain                     Link keychain for all existing instances
    agx work keychain                Link keychain for the 'work' account
    agx usage                        Show beautiful usage tables across all accounts
    agx work usage                   Show usage quota for the 'work' account
    agx usage work                   Show usage quota for the 'work' account
    agx ping                         Ping and check health across all accounts
    agx work ping                    Ping and check health of the 'work' account
    agx work                         Run agy inside ~/.agx/work
    agx work -d                      Run agy with --dangerously-skip-permissions
    agx work disable                 Disable the 'work' account
    agx work enable                  Re-enable the 'work' account
    agx work chat "hello world"      Run agy passing arguments
    agx add personal                 Prepare ~/.agx/personal
    agx list                         Show all available instances
"#
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        return ExitCode::from(1);
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "keychain" => {
            let res = if args.len() >= 2 {
                handle_keychain(Some(&args[1]))
            } else {
                handle_keychain(None)
            };
            if let Err(err) = res {
                eprintln!("Error: {:#}", err);
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        "usage" => {
            let res = if args.len() >= 2 {
                handle_instance_usage(&args[1])
            } else {
                handle_usage()
            };
            if let Err(err) = res {
                eprintln!("Error: {:#}", err);
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        "ping" => {
            let res = if args.len() >= 2 {
                handle_ping_instance(&args[1])
            } else {
                handle_ping_all()
            };
            if let Err(err) = res {
                eprintln!("Error: {:#}", err);
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        "add" => {
            if args.len() < 2 {
                eprintln!("Error: 'agx add' requires an instance name.\nUsage: agx add <instance-name>");
                return ExitCode::from(1);
            }
            if let Err(err) = handle_add(&args[1]) {
                eprintln!("Error: {:#}", err);
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        "list" | "ls" => {
            if let Err(err) = handle_list() {
                eprintln!("Error: {:#}", err);
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        instance_name => {
            if args.len() == 2 && args[1] == "keychain" {
                if let Err(err) = handle_keychain(Some(instance_name)) {
                    eprintln!("Error: {:#}", err);
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }
            if args.len() == 2 && args[1] == "usage" {
                if let Err(err) = handle_instance_usage(instance_name) {
                    eprintln!("Error: {:#}", err);
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }
            if args.len() == 2 && args[1] == "enable" {
                if let Err(err) = handle_enable(instance_name) {
                    eprintln!("Error: {:#}", err);
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }
            if args.len() == 2 && args[1] == "disable" {
                if let Err(err) = handle_disable(instance_name) {
                    eprintln!("Error: {:#}", err);
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }
            if args.len() == 2 && args[1] == "ping" {
                if let Err(err) = handle_ping_instance(instance_name) {
                    eprintln!("Error: {:#}", err);
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }

            let forwarded_args = &args[1..];
            match handle_run(instance_name, forwarded_args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("Error: {:#}", err);
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_usage_tab_format() {
        let input = "Gemini Models\tWeekly Limit Remaining\t68%\t2026-10-18T07:32:48Z\nGemini Models\tFive Hour Limit Remaining\t98%\t2026-10-13T10:31:36Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-20T05:35:52Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-10-13T10:35:52Z";
        let metrics = parse_usage_output(input);
        assert_eq!(metrics.len(), 4);
        assert_eq!(metrics[0].model_family, "Gemini Models");
        assert_eq!(metrics[0].limit_type, "Weekly Limit Remaining");
        assert_eq!(metrics[0].remaining_pct, 68);
        assert_eq!(metrics[0].reset_time, "2026-10-18T07:32:48Z");
    }

    #[test]
    fn test_parse_usage_spaces_format() {
        let input = r#"
Quota:
Gemini Models          Weekly Limit Remaining     68%   2026-10-18 11:02 +0330
Gemini Models          Five Hour Limit Remaining  98%   2026-10-13 14:01 +0330
Claude and GPT models  Weekly Limit Remaining     100%  2026-10-20 09:04 +0330
Claude and GPT models  Five Hour Limit Remaining  100%  2026-10-13 14:04 +0330
"#;
        let metrics = parse_usage_output(input);
        assert_eq!(metrics.len(), 4);
        assert_eq!(metrics[0].model_family, "Gemini Models");
        assert_eq!(metrics[0].limit_type, "Weekly Limit Remaining");
        assert_eq!(metrics[0].remaining_pct, 68);
        assert_eq!(metrics[0].reset_time, "2026-10-18 11:02 +0330");
        assert_eq!(metrics[2].model_family, "Claude and GPT models");
        assert_eq!(metrics[2].remaining_pct, 100);
    }

    #[test]
    fn test_parse_usage_json_format() {
        let input = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t68%\t2026-10-18T07:32:48Z\nGemini Models\tFive Hour Limit Remaining\t98%\t2026-10-13T10:31:36Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-20T05:39:49Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-10-13T10:39:49Z\n","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"command":{"name":"usage","data":{"description":"Within each group...","groups":[{"name":"Gemini Models","description":"Models within this group","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"You have used some of your weekly limit","window":"weekly","remaining_fraction":0.6833860278129578,"reset_time":"2026-10-18T07:32:48Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","description":"You have used some of your 5-hour limit","window":"5h","remaining_fraction":0.9785395860671997,"reset_time":"2026-10-13T10:31:36Z"}]},{"name":"Claude and GPT models","description":"Models within this group","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1.0,"reset_time":"2026-10-20T05:39:49Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1.0,"reset_time":"2026-10-13T10:39:49Z"}]}]}}}"#;
        let metrics = parse_usage_output(input);
        assert_eq!(metrics.len(), 4);
        assert_eq!(metrics[0].model_family, "Gemini Models");
        assert_eq!(metrics[0].limit_type, "Weekly Limit Remaining");
        assert_eq!(metrics[0].remaining_pct, 68);
        assert_eq!(metrics[0].reset_time, "2026-10-18T07:32:48Z");
        assert_eq!(metrics[1].remaining_pct, 98);
        assert_eq!(metrics[2].model_family, "Claude and GPT models");
        assert_eq!(metrics[2].remaining_pct, 100);
    }

    #[test]
    fn test_human_relative_time() {
        let desc = "You have used some of your weekly limit, it will fully refresh in 5 days, 1 hour.";
        let res = format_human_relative_time("invalid_ts", Some(desc));
        assert_eq!(res, Some("in 5d 1h".to_string()));

        let dur = format_relative_seconds(3600 * 4 + 60 * 30 + 15);
        assert_eq!(dur, "in 4h 30min 15s");

        let dur_no_sec = format_relative_seconds(3600 * 4 + 60 * 30);
        assert_eq!(dur_no_sec, "in 4h 30min");

        let dur_days = format_relative_seconds(86400 * 3 + 3600 * 2 + 60 * 5 + 42);
        assert_eq!(dur_days, "in 3d 2h 5min 42s");

        let dur_min = format_relative_seconds(60 * 15 + 8);
        assert_eq!(dur_min, "in 15min 8s");

        let dur_sec_only = format_relative_seconds(45);
        assert_eq!(dur_sec_only, "in 45s");

        let dur_zero = format_relative_seconds(0);
        assert_eq!(dur_zero, "now");
    }

    #[test]
    fn test_render_progress_bar() {
        let bar_100 = render_progress_bar(100, 10);
        assert_eq!(bar_100, "[██████████] 100%");
        let bar_50 = render_progress_bar(50, 10);
        assert_eq!(bar_50, "[█████░░░░░]  50%");
        let bar_0 = render_progress_bar(0, 10);
        assert_eq!(bar_0, "[░░░░░░░░░░]   0%");
    }

    #[test]
    fn test_ping_result_structure() {
        let res = PingResult {
            instance_name: "ramon".to_string(),
            status: HealthStatus::Healthy,
            latency: Some(std::time::Duration::from_millis(1250)),
            message: "2".to_string(),
        };
        assert_eq!(res.status, HealthStatus::Healthy);
        assert_eq!(res.instance_name, "ramon");
    }

    #[test]
    fn test_get_keychain_link_targets() {
        let dummy_home = PathBuf::from("/tmp/dummy_home");
        let targets = get_keychain_link_targets(&dummy_home);
        if cfg!(target_os = "macos") {
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].0, "macOS Keychain");
            assert_eq!(targets[0].1, dummy_home.join("Library/Keychains"));
        } else if cfg!(target_os = "linux") {
            assert_eq!(targets.len(), 3);
            assert_eq!(targets[0].0, "GNOME Keyring");
            assert_eq!(targets[0].1, dummy_home.join(".local/share/keyrings"));
            assert_eq!(targets[1].0, "KDE KWallet data");
            assert_eq!(targets[1].1, dummy_home.join(".local/share/kwalletd"));
            assert_eq!(targets[2].0, "KDE KWallet config");
            assert_eq!(targets[2].1, dummy_home.join(".config/kwalletrc"));
        }
    }

    #[test]
    fn test_link_single_keychain_entry() {
        let temp_dir = std::env::temp_dir().join(format!("agx_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&temp_dir);

        let source = temp_dir.join("source_keychain");
        let target = temp_dir.join("target_instance/keychain");

        // Source doesn't exist yet -> should return false
        let res = link_single_keychain_entry("test", &source, &target).unwrap();
        assert!(!res);

        // Create source directory
        fs::create_dir_all(&source).unwrap();

        // Should link successfully
        let res = link_single_keychain_entry("test", &source, &target).unwrap();
        assert!(res);
        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), source);

        // Running again when already linked -> should return false without error
        let res2 = link_single_keychain_entry("test", &source, &target).unwrap();
        assert!(!res2);

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}

