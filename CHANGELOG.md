# What's new

Each version's notes are shown inside the app (after an update, and in the update bar) and become the GitHub release notes. Add a `## x.y.z` section before publishing a version.

## 0.4.0

- **Settings window** (the Settings button, or Ctrl+,): mouse-wheel scroll speed, theme (system / dark / light), interface size, your comment author name, whether tabs reopen on start, and the AI and update options in one place.
- **Updates inside the app.** LeanPDF checks GitHub once a day (you can turn that off) and shows a bar when a new version is out. **Update now** downloads it, checks it, installs it and starts LeanPDF again; nothing to download by hand. **What's new** shows what changed.
- **Pasting works.** The clipboard was missing, so pasting an API key (and copying text) did not work. Both do now.
- **Pick the AI model from a list.** The dropdown is filled from OpenRouter or Anthropic (with prices for OpenRouter) and can be filtered. The default is now Claude Haiku 5.5: fast and inexpensive for answering questions about a PDF; choose Sonnet or Opus for harder questions.
- **Faster scrolling** in the page view, adjustable in Settings.

## 0.3.0

- **Ask AI** (Ctrl+J): ask questions about the open PDF, or the whole folder, with your own OpenRouter or Anthropic key. LeanPDF sends only the pages needed, streams the answer and lists the pages it used as clickable sources.

## 0.2.0

- **Tabs**: every PDF opens in its own tab; tabs come back when you restart.
- A smaller, cleaner arrow for folders in the sidebar.
- One app icon everywhere: window, taskbar, installer and shortcuts.

## 0.1.0

- First release: folder-wide text search, a folder tree, comments that Adobe Acrobat can read (highlight, underline, strike through, sticky notes), and a Windows installer.
