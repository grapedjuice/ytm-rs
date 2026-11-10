<!--
Title: what changes for someone using the app, with the area first if it helps.
  Good: "Song lists: a click no longer starts a drag"
  Not:  "Fix row_drag.rs" / "Update audio"

One PR, one change. Lines inside these comment marks don't show up; delete any section that doesn't apply.
-->

<!-- 1. What was wrong or missing, and what the app does now, in a sentence or three. Write it for a listener, not for the code. Link the issue if there is one: "Fixes #13" or "Refs #70". -->


### Changes

<!-- What you changed and where, one bullet per change, with the file or function. Say why wherever the code alone doesn't.

- **Frames are capped to the screen's refresh rate** (`src/ui/pace.rs`). Frames are at least 0.9 of a refresh apart, so a working vsync still sets the pace.
-->

-

### Checked

<!-- What you actually ran and did. "It builds" is not a check: say what you clicked or played in the running app, and what happened. For a bug, how you reproduced it before the change. For a speed or memory claim, the numbers before and after. -->

- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- Ran the app on: <!-- Windows 11 / macOS 15 (Apple silicon) / Linux (distro, desktop, X11 or Wayland) -->
- Account: <!-- signed out / signed in without Premium / signed in with Premium. Playback, library and likes behave differently for each. -->
- What I tried:

### Screenshots

<!-- For anything you can see: before and after, or a short clip. Delete this section otherwise. -->

### Not covered

<!-- Anything a reviewer might expect that this PR leaves out: platforms you couldn't test, known limits, costs (more network, CPU or memory), follow-ups. Delete if there's nothing. -->
