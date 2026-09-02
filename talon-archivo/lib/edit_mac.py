"""Implementacion de las acciones edit.* para macOS.

El core de Talon DECLARA edit.copy, edit.select_all, etc. pero las deja
vacias: quien las implementa normalmente es talonhub/community. Sin el,
cualquier comando que las use falla con NotImplementedError.

Aqui las rellenamos con los atajos estandar de macOS. Ventaja frente a
poner key() suelto en los .talon: una app concreta puede sobrescribir
solo la accion que se comporte distinto, sin tocar los comandos.
"""

from talon import Context, actions

ctx = Context()
ctx.matches = "os: mac"

key = actions.key


@ctx.action_class("edit")
class EditActions:
    # --- Portapapeles ---
    def copy():
        key("cmd-c")

    def cut():
        key("cmd-x")

    def paste():
        key("cmd-v")

    def paste_match_style():
        key("cmd-shift-alt-v")

    # --- Archivo e historial ---
    def save():
        key("cmd-s")

    def save_all():
        key("cmd-alt-s")

    def undo():
        key("cmd-z")

    def redo():
        key("cmd-shift-z")

    # --- Borrado ---
    def delete():
        key("backspace")

    def delete_word():
        key("alt-backspace")

    def delete_line():
        key("cmd-left cmd-shift-right backspace backspace")

    # --- Movimiento del cursor ---
    def left():
        key("left")

    def right():
        key("right")

    def up():
        key("up")

    def down():
        key("down")

    def word_left():
        key("alt-left")

    def word_right():
        key("alt-right")

    def line_start():
        key("cmd-left")

    def line_end():
        key("cmd-right")

    def file_start():
        key("cmd-up")

    def file_end():
        key("cmd-down")

    def page_up():
        key("pageup")

    def page_down():
        key("pagedown")

    # --- Seleccion ---
    def select_all():
        key("cmd-a")

    def select_none():
        key("right")

    def select_line(n: int = None):
        # n (ir a una linea concreta) necesitaria edit.jump_line, que
        # depende del editor y no esta implementada. Nuestros comandos
        # nunca lo pasan: siempre selecciona la linea actual.
        key("cmd-left cmd-shift-right")

    def select_word():
        key("alt-right alt-shift-left")

    def extend_left():
        key("shift-left")

    def extend_right():
        key("shift-right")

    def extend_up():
        key("shift-up")

    def extend_down():
        key("shift-down")

    def extend_word_left():
        key("alt-shift-left")

    def extend_word_right():
        key("alt-shift-right")

    def extend_line_start():
        key("cmd-shift-left")

    def extend_line_end():
        key("cmd-shift-right")

    def extend_file_start():
        key("cmd-shift-up")

    def extend_file_end():
        key("cmd-shift-down")

    # --- Buscar ---
    def find(text: str = None):
        key("cmd-f")
        if text:
            actions.insert(text)

    def find_next():
        key("cmd-g")

    def find_previous():
        key("cmd-shift-g")

    # --- Indentacion y zoom ---
    # Teclado espanol ISO: los corchetes y el '=' no son teclas fisicas,
    # asi que cmd-] / cmd-[ / cmd-= no llegan. tab, plus y minus si.
    def indent_more():
        key("tab")

    def indent_less():
        key("shift-tab")

    def zoom_in():
        key("cmd-plus")

    def zoom_out():
        key("cmd-minus")

    def zoom_reset():
        key("cmd-0")
