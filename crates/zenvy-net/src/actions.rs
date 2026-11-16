//! Custom actions from the config: a fixed program and arguments (never a shell line), run on request with a
//! timeout. A phone only ever sends an action *id*; the argument vector never leaves the agent.

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;

const RUN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDef {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub confirm: bool,
    pub argv: Vec<String>,
}

/// What phones are told about the actions: no argument vector.
pub fn public_view(actions: &[ActionDef]) -> Value {
    Value::Array(
        actions
            .iter()
            .map(|a| json!({ "id": a.id, "label": a.label, "icon": a.icon, "confirm": a.confirm }))
            .collect(),
    )
}

/// Runs the action. `Err` carries a short reason (never the command line).
pub async fn run(a: &ActionDef) -> Result<(), String> {
    let (prog, args) = a.argv.split_first().ok_or("empty command")?;
    let child = tokio::process::Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start: {}", e.kind()))?;
    match tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output()).await {
        Err(_) => Err("took longer than 30 seconds and was stopped".into()),
        Ok(Err(e)) => Err(format!("could not run: {}", e.kind())),
        Ok(Ok(o)) if o.status.success() => Ok(()),
        Ok(Ok(o)) => {
            let err: String = String::from_utf8_lossy(&o.stderr)
                .chars()
                .filter(|c| !c.is_control() || *c == ' ')
                .take(120)
                .collect();
            Err(if err.trim().is_empty() {
                format!("exited with {}", o.status.code().unwrap_or(-1))
            } else {
                err.trim().to_string()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(argv: &[&str]) -> ActionDef {
        ActionDef {
            id: "t".into(),
            label: "T".into(),
            icon: None,
            confirm: false,
            argv: argv.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_public_view_never_contains_the_command() {
        let a = ActionDef {
            argv: vec!["/usr/bin/secret-tool".into(), "--token=hunter2".into()],
            icon: Some("zap".into()),
            confirm: true,
            ..def(&["x"])
        };
        let v = public_view(&[a]).to_string();
        assert!(
            v.contains("\"id\":\"t\"")
                && v.contains("\"confirm\":true")
                && !v.contains("hunter2")
                && !v.contains("secret-tool")
                && !v.contains("argv"),
            "{v}"
        );
    }

    #[tokio::test]
    async fn runs_without_a_shell_and_reports_failures_briefly() {
        assert!(run(&def(&["true"])).await.is_ok());
        let e = run(&def(&["false"])).await.unwrap_err();
        assert!(e.contains("exited with 1"), "{e}");
        // shell metacharacters are plain arguments: nothing is interpreted, so this is just a missing file
        let e = run(&def(&[
            "ls",
            "/nonexistent; touch /tmp/zv-should-not-exist",
        ]))
        .await
        .unwrap_err();
        assert!(
            !std::path::Path::new("/tmp/zv-should-not-exist").exists(),
            "{e}"
        );
        let e = run(&def(&["/nonexistent/program"])).await.unwrap_err();
        assert!(e.starts_with("could not start"), "{e}");
        assert!(run(&def(&[])).await.is_err());
    }
}
