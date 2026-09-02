"""Enfocar o lanzar aplicaciones por voz.

Sustituye a user.switcher_focus de talonhub/community, que aqui no existe.
La lista user.launch_app mapea la palabra hablada -> bundle id de macOS.
"""

from talon import Module, ui

mod = Module()
mod.list("launch_app", desc="Apps que se pueden enfocar o lanzar por voz")


def _running(bundle: str):
    """Devuelve la app en ejecucion con ese bundle id, o None."""
    for app in ui.apps(background=False):
        if app.bundle == bundle:
            return app
    return None


def _has_usable_window(app) -> bool:
    """True si la app tiene alguna ventana que se pueda enfocar.

    En macOS cerrar la ultima ventana no cierra la app: puede quedarse
    corriendo con cero ventanas, o solo con ventanas minimizadas. En ambos
    casos app.focus() cambia la barra de menus y no muestra nada.
    """
    try:
        windows = app.windows()
    except Exception:
        return False
    return any(not getattr(w, "hidden", False) for w in windows)


@mod.action_class
class Actions:
    def focus_app(bundle: str):
        """Enfoca la app; la lanza o le pide ventana si no tiene ninguna"""
        app = _running(bundle)
        if app and _has_usable_window(app):
            app.focus()
        else:
            # ui.launch hace lo mismo que pulsar el icono del Dock: lanza la
            # app si esta cerrada, y si ya corre le pide que cree una ventana.
            ui.launch(bundle=bundle)
