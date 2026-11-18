# Translating ytm-rs

Thanks for helping! The app's own text (its buttons, menus, settings and notices) lives in `locales/`, one folder per language. English, in `locales/en-US/`, is the source.

So far the Settings page is ready to translate. More of the app follows, a screen at a time.

## How to translate

1. Copy `locales/en-US/` to a folder named after your language, for example `locales/pt-BR/`, `locales/es-ES/` or `locales/id-ID/`. If the folder is already there, open it.
2. In `app.ftl`, set `language-name` to your language's name in itself (`Português`, `Español`, `Bahasa Indonesia`).
3. Translate the text after each `=`. Leave the id before it (`settings-keep-on-top`) as it is.
4. Open a pull request. One file at a time is fine.

A browser-based way to translate, with no Git needed, is coming.

## What the files look like

```
settings-keep-on-top = Keep on top
settings-keep-on-top-desc = The mini player stays above other windows.

likes-removed = { $count ->
    [one] Removed { $count } like
   *[other] Removed { $count } likes
}
```

- Lines starting with `#` are notes for you; they don't need translating.
- `{ $count }`, `{ $name }` and the like are filled in by the app. Keep them as they are; you can move them anywhere in the sentence.
- Lines with `[one]` and `*[other]` pick a wording by number. Give the forms your language uses: Indonesian needs only `*[other]`; Arabic uses `[zero]`, `[one]`, `[two]`, `[few]`, `[many]` and `*[other]`. Keep the `*` on the last one. The [Fluent guide](https://projectfluent.org/fluent/guide/selectors.html) has more.
- Anything you haven't translated yet shows in English, so a partly done language still works.

## Tips

- Keep it short. Buttons and row titles have limited room, so a shorter word that means the same thing is better.
- Use the words your language's other apps use for things like "queue", "playlist" and "shuffle".
- Song titles, artist names and YouTube Music's shelves ("Quick picks") come from YouTube Music, not the app, so you won't find them here.

## Trying your translation

You need [Rust](https://rustup.rs) to build the app from source:

```
cargo run
```

Then pick your language in **Settings › General › Language** and restart. A language shows in that list once it's added to `LANGUAGES` in `src/i18n.rs`, which happens when it's mostly translated. Add it there yourself to try it.

`cargo test i18n` checks that every file reads correctly.

To check the app has room for longer text, start it with `YTM_PSEUDO_LOCALE=1`: every translated message shows stretched and bracketed, like `[Ķééƥ ŏñ ţŏƥ ~~~~]`. Text still in plain English isn't translatable yet; text cut off has too little room. Both are worth an issue.
