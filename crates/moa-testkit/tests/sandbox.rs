use moa_testkit::Sandbox;

#[test]
fn runs_a_supported_tmux() {
    let sb = Sandbox::start().unwrap();
    let v = sb.tmux(&["display-message", "-p", "#{version}"]).unwrap();
    // "3.4", "3.8", "3.5a", "next-3.9": take the first "major.minor" digits
    let digits: String = v
        .trim_start_matches("next-")
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut it = digits.split('.').map(|n| n.parse::<u32>().unwrap());
    let (major, minor) = (it.next().unwrap(), it.next().unwrap());
    assert!((major, minor) >= (3, 4), "tmux {v} is older than 3.4");
}

#[test]
fn socket_path_fits() {
    let sb = Sandbox::start().unwrap();
    let p = sb
        .tmux(&["display-message", "-p", "#{socket_path}"])
        .unwrap();
    assert!(p.len() < 104, "socket path is {} bytes: {p}", p.len());
    assert!(p.ends_with(sb.socket_name()));
}

#[test]
fn home_shell_and_config_are_the_sandboxes() {
    let sb = Sandbox::start().unwrap();
    let home = sb.tmux(&["show-environment", "-g", "HOME"]).unwrap();
    assert_eq!(home, format!("HOME={}", sb.home().display()));
    assert_ne!(
        sb.home(),
        std::path::Path::new(&std::env::var("HOME").unwrap())
    );
    // the sandbox conf, not the user's: /bin/sh panes and tmux's default prefix
    assert_eq!(
        sb.tmux(&["show", "-gv", "default-shell"]).unwrap(),
        "/bin/sh"
    );
    assert_eq!(sb.tmux(&["show", "-gv", "prefix"]).unwrap(), "C-b");
    // proves -f tmux.conf was actually loaded
    assert_eq!(sb.tmux(&["show", "-gv", "@moa-sandbox"]).unwrap(), "1");
    // panes start in the sandbox home, not in the repo the tests run from
    assert_eq!(
        sb.tmux(&[
            "display-message",
            "-p",
            "-t",
            "sandbox",
            "#{pane_current_path}"
        ])
        .unwrap(),
        std::fs::canonicalize(sb.home())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn server_is_gone_after_drop() {
    let sb = Sandbox::start().unwrap();
    let pid: u32 = sb
        .tmux(&["display-message", "-p", "#{pid}"])
        .unwrap()
        .parse()
        .unwrap();
    drop(sb);
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "tmux server {pid} still running after drop");
}

#[test]
fn two_sandboxes_do_not_share_a_server() {
    let (a, b) = (Sandbox::start().unwrap(), Sandbox::start().unwrap());
    assert_ne!(a.socket_name(), b.socket_name());
    let pa = a.tmux(&["display-message", "-p", "#{pid}"]).unwrap();
    let pb = b.tmux(&["display-message", "-p", "#{pid}"]).unwrap();
    assert_ne!(pa, pb);
}
