# What's new

Each version's notes are shown inside the app (after an update, and in the update bar) and become the GitHub release notes. Add a `## x.y.z` section before publishing a version.

## 0.5.0

- **Find in this document** (Ctrl+F). Every match is highlighted on the pages; Enter or F3 goes to the next one and Shift+Enter to the previous one, with a count such as "3 of 17".
- **Outline and Pages.** The left panel now has Files, Outline (the PDF's own bookmarks) and Pages (thumbnails) tabs. Click an entry or a thumbnail to jump there.
- **Text boxes.** The new **Text box** button (or the T key) draws a text box: drag a rectangle on the page, then type. Text boxes from Acrobat can be edited too, and Acrobat shows the new text.
- **Long comments are easy to save.** The comment window gets wider for long text, the text scrolls inside it, and Save and Cancel are always on screen. Long comments scroll inside their card in the Comments panel, so Edit, Copy and Delete stay in reach.
- **Pick up where you left off.** The File menu lists your recent files, and a file you open again continues at the page you were reading.
- **Rotate view** (Ctrl+R, or Ctrl+Shift+R the other way) turns the pages without changing the file, and **Print** (Ctrl+P) uses the standard Windows print dialog.
- **One-click AI questions.** Ask AI has Summarize, Key points, Quiz me and Explain this page buttons above the question box.

## 0.4.4

- **Page number on the page.** A small "Page 5 / 20" label floats over the bottom of the page view and updates as you scroll, so you always know where you are without looking up at the toolbar. Move it to the bottom right or top right, or turn it off, under Settings, Page number on the page.

## 0.4.3

- **Drag tabs to reorder them**, like in Acrobat. Press and hold a tab and drag it left or right; the other tabs make room as you pass them. The new order is remembered when you restart.

## 0.4.2

- **Edit Adobe comments.** Comments written in Adobe Acrobat can now be edited (and copied or deleted) in LeanPDF. The Edit, Copy and Delete buttons are always shown under each comment instead of appearing only on hover, and double-clicking a comment, in the panel or on the page, opens it for editing. Clicking a comment on the page selects it in the panel. Text boxes, stamps and attachments are still view-only.
- **Tables and code look right.** Comments you pasted into Acrobat now keep their line breaks and tab stops and show in a monospace box, so tables line up and code keeps its indentation. They also stay correct when opened again in Acrobat after an edit in LeanPDF: Acrobat's rich-text copy of the comment is updated together with the plain text, so an edit no longer shows the old text.

## 0.4.1

- **Fixed:** "Could not reach OpenRouter: native-tls: unable to find any user-specified roots in the final cert chain". LeanPDF now uses Windows' own certificate store for secure connections, so the AI model list, AI answers and the update check all connect. If you saw that error, install this version once by hand; after that updates install themselves.

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
