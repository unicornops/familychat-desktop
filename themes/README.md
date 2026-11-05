# Themes

Theme files in this directory use unchanged Zed theme JSON format, read by the compatibility layer in `crates/familychat-theme` (copied from Zmail's `zmail-theme`), so Family Chat can load Zed-style theme families without depending on the full Zed workspace.

`familychat-light.json` and `familychat-dark.json` are the bundled Family Chat themes. The light theme takes the website's palette (`website/static/css/style.css` in unicornops/family-chat: teal `#0D9488` primary, orange `#F97316` accent, warm stone neutrals); the dark theme is its counterpart on the same Tailwind teal, orange and stone scales. The window follows the OS appearance until the user can pick a theme.
