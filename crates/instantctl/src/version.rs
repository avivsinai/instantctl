pub const GIT_SHA: &str = env!("INSTANTCTL_GIT_SHA");
pub const DISPLAY: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (git ",
    env!("INSTANTCTL_GIT_SHA"),
    ")"
);
