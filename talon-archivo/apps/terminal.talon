os: mac
app.bundle: com.apple.Terminal
-
# --- Pestanas y ventanas ---
tab new: key(cmd-t)
tab close: key(cmd-w)
tab next: key(ctrl-tab)
tab previous: key(ctrl-shift-tab)

# --- Control de la sesion ---
clear it: key(ctrl-l)
cancel it: key(ctrl-c)
history back: key(up)
history next: key(down)

# --- Atajos de linea (emacs, los que trae bash/zsh por defecto) ---
line home: key(ctrl-a)
line ending: key(ctrl-e)
kill line: key(ctrl-k)

# --- Comandos escritos, SIN pulsar enter ---
# Se quedan en el prompt para que los revises antes de ejecutar.
list files: "ls -la"
show status: "git status"
show branch: "git branch"
show diff: "git diff"
show log: "git log --oneline -20"
show docker: "docker ps"
