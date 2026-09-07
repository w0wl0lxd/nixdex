- Declare a released minimum Rust version. The workspace asked for `1.99.0`,
  which does not exist yet, so `cargo` refused to build on stable and on beta.
  The floor is now `1.90.0`, the lowest released toolchain that builds the
  whole workspace with every feature on.
- Keep `unsafe_code` denied outside `nixdex-core::database::huge_pages`. The
  lint had been relaxed to `warn` for every crate to admit two `madvise` calls;
  it is `deny` again, and only that module opts out.
- Replace the deprecated `serde_yaml` with the maintained `serde_norway` fork.
