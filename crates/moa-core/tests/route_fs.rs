use std::path::Path;

use moa_core::config::{AccountName, BrainName, Config};
use moa_core::route::{Route, route};

#[test]
fn a_symlinked_folder_routes_like_its_target() {
    let tmp = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("real/sub")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

    let text = format!(
        "[brains.work]\n[accounts.acct-a]\nkind = \"claude\"\nstore = \"{r}/store\"\nbrain = \"work\"\nexcept = [\"{r}/link\"]\n",
        r = root.display()
    );
    let c = Config::from_file_text(Some(&text), Path::new("/h"), moa_platform::canonical).unwrap();
    let a = AccountName::parse("acct-a").unwrap();
    let opener = BrainName::parse("opener").unwrap();

    let cwd = std::fs::canonicalize(root.join("link/sub")).unwrap();
    assert_eq!(
        route(&c, Some(&a), &cwd, Some(&opener)),
        Route::Brain(opener.clone())
    );
    // Outside the folder the account's own brain wins.
    let outside = std::fs::canonicalize(root).unwrap();
    assert_eq!(
        route(&c, Some(&a), &outside, Some(&opener)),
        Route::Brain(BrainName::parse("work").unwrap())
    );
}
