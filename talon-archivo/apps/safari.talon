os: mac
app.bundle: com.apple.Safari
-
# --- Pestanas ---
tab new: key(cmd-t)
tab close: key(cmd-w)
tab restore: key(cmd-shift-t)
tab next: key(ctrl-tab)
tab previous: key(ctrl-shift-tab)
tab one: key(cmd-1)
tab two: key(cmd-2)
tab three: key(cmd-3)
tab four: key(cmd-4)
tab five: key(cmd-5)
tab final: key(cmd-9)

# --- Navegacion ---
# Teclado espanol ISO: cmd-[ y cmd-] mandan un keycode que aqui no existe.
# cmd-left / cmd-right son los atajos nativos y no dependen del layout.
page back: key(cmd-left)
page forward: key(cmd-right)
page reload: key(cmd-r)
page topper: key(cmd-up)
page bottom: key(cmd-down)
address bar: key(cmd-l)
find here: key(cmd-f)

# --- Zoom ---
# 'plus' y 'minus' se resuelven segun el layout activo; '=' y '-' no.
zoom more: key(cmd-plus)
zoom less: key(cmd-minus)
zoom reset: key(cmd-0)
