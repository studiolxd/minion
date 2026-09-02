-
# --- Portapapeles y deshacer ---
copy it: edit.copy()
paste it: edit.paste()
cut it: edit.cut()
save it: edit.save()
undo it: edit.undo()
redo it: edit.redo()
select all: edit.select_all()

# --- Borrado ---
scratch it: key(backspace)
delete word: edit.delete_word()

# --- Navegacion de texto ---
word left: edit.word_left()
word right: edit.word_right()
line home: edit.line_start()
line ending: edit.line_end()
file topper: edit.file_start()
file bottom: edit.file_end()

# --- Seleccion ---
select word: edit.select_word()
select line: edit.select_line()
