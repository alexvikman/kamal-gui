//! `kgui` command line. The same entry point is used by the standalone CLI
//! and by the app binary (so `.kamal/secrets` can call `kgui secret` while
//! the app runs kamal).

use crate::settings::Settings;
use crate::{project, secrets, status};
use anyhow::{Context, Result, bail};

const USAGE: &str = "kgui — Kamal GUI command line

  kgui secret op://VAULT/ITEM/FIELD       Print a secret (for .kamal/secrets)
  kgui migrate-secrets [--write] FILE…    Replace op-read-item-field calls with kgui secret
  kgui projects                           List projects and their deploy targets
  kgui add GIT_URL                        Add a project (clones it)
  kgui remove PROJECT                     Remove a project and its clone
  kgui fetch PROJECT                      Fetch a project from origin
  kgui status PROJECT                     Show containers per host
  kgui doctor                             Show which kamal, op and gh will be used
";

/// Handle a CLI invocation. Returns None when the arguments are not a CLI
/// subcommand (the app binary then starts the GUI).
pub fn dispatch(args: &[String]) -> Option<i32> {
    let sub = args.get(1)?.as_str();
    let rest = &args[2..];
    let result = match sub {
        "secret" => secret(rest),
        "migrate-secrets" => migrate(rest),
        "projects" => projects(),
        "add" => add(rest),
        "remove" => remove(rest),
        "fetch" => fetch(rest),
        "status" => status_cmd(rest),
        "doctor" => doctor(),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        _ => return None,
    };
    Some(match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kgui: {e:#}");
            1
        }
    })
}

fn one<'a>(args: &'a [String], usage: &str) -> Result<&'a str> {
    match args {
        [a] => Ok(a),
        _ => bail!("usage: {usage}"),
    }
}

fn secret(args: &[String]) -> Result<()> {
    let reference = one(args, "kgui secret op://VAULT/ITEM/FIELD")?;
    let account = std::env::var("KGUI_OP_ACCOUNT").unwrap_or_else(|_| Settings::load().op_account);
    let value = secrets::lookup(reference, &account)?;
    // `$(…)` strips trailing newlines anyway; print the value as-is.
    print!("{value}");
    Ok(())
}

fn migrate(args: &[String]) -> Result<()> {
    let write = args.iter().any(|a| a == "--write");
    let files: Vec<&String> = args.iter().filter(|a| *a != "--write").collect();
    if files.is_empty() {
        bail!("usage: kgui migrate-secrets [--write] .kamal/secrets…");
    }
    for f in files {
        let text = std::fs::read_to_string(f).with_context(|| format!("can't read {f}"))?;
        match secrets::migrate_legacy(&text) {
            None => println!("{f}: nothing to change"),
            Some(new) => {
                let n = new.matches("kgui secret").count() - text.matches("kgui secret").count();
                if write {
                    std::fs::write(f, &new)?;
                    println!("{f}: migrated {n} secrets");
                } else {
                    println!("{f}: would migrate {n} secrets (run with --write)");
                    for l in new.lines().filter(|l| l.contains("kgui secret")).take(2) {
                        println!("  {l}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn projects() -> Result<()> {
    for p in project::list() {
        match project::load_detail(p.clone()) {
            Ok(detail) => {
                let targets: Vec<String> = detail
                    .targets
                    .iter()
                    .map(|t| {
                        let kinds: Vec<&str> = t.units.iter().map(|u| u.kind.as_str()).collect();
                        format!("{}[{}]", t.id, kinds.join("+"))
                    })
                    .collect();
                println!("{:<36} {:<14} {}", p.name, detail.default_branch, targets.join(" "));
            }
            Err(e) => println!("{:<36} error: {e:#}", p.name),
        }
    }
    Ok(())
}

fn add(args: &[String]) -> Result<()> {
    let p = project::add(one(args, "kgui add GIT_URL")?)?;
    println!("added {} ({})", p.name, p.url);
    Ok(())
}

fn remove(args: &[String]) -> Result<()> {
    let name = one(args, "kgui remove PROJECT")?;
    project::remove(name)?;
    println!("removed {name}");
    Ok(())
}

fn fetch(args: &[String]) -> Result<()> {
    let name = one(args, "kgui fetch PROJECT")?;
    project::fetch(name)?;
    let d = project::load_detail(project::find(name)?)?;
    if let Some(c) = d.commit {
        println!("{name}: {} {} {}", d.default_branch, c.short, c.subject);
    }
    Ok(())
}

fn status_cmd(args: &[String]) -> Result<()> {
    let name = one(args, "kgui status PROJECT")?;
    let detail = project::load_detail(project::find(name)?)?;
    let st = status::project_status(&detail);
    for h in &st.hosts {
        match &h.error {
            Some(e) => println!("{}: ERROR {e}", h.host),
            None => {
                println!("{}:", h.host);
                for c in &h.containers {
                    let info = st.versions.get(&c.version);
                    let behind = info.and_then(|i| i.behind).map(|b| format!(" ({b} behind)")).unwrap_or_default();
                    println!(
                        "  {:<8} {:<40} {:<8} {}{}",
                        c.role.as_deref().unwrap_or("-"),
                        c.name,
                        c.state,
                        &c.version[..c.version.len().min(12)],
                        behind
                    );
                }
            }
        }
    }
    Ok(())
}

fn doctor() -> Result<()> {
    use crate::env::{command, run};
    let settings = Settings::load();
    let kamal = crate::kamal::binary(&settings);
    let version = |cmd: &mut std::process::Command| run(cmd).map(|v| v.lines().next().unwrap_or("").to_string());
    let kamal_version = version(command(&kamal).arg("version")).unwrap_or_else(|e| format!("error: {e:#}"));
    println!("kamal   {} ({kamal_version})", kamal.display());
    for (name, args) in
        [("op", vec!["--version"]), ("gh", vec!["--version"]), ("git", vec!["--version"]), ("ssh", vec!["-V"])]
    {
        let out = command(name).args(&args).output();
        let line = match out {
            Ok(o) => {
                let text = if o.stdout.is_empty() { o.stderr } else { o.stdout };
                String::from_utf8_lossy(&text).lines().next().unwrap_or("").to_string()
            }
            Err(e) => format!("not found ({e})"),
        };
        println!("{name:<7} {line}");
    }
    println!("data    {}", crate::settings::data_dir().display());
    println!("1Password account: {}", settings.op_account);
    Ok(())
}
