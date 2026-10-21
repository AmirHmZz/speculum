/// APK packages have versioned filenames; indexes and other repository data may change.
pub fn classify(path: &str) -> (bool, bool) {
    let package = path.ends_with(".apk");
    (package, !package)
}
