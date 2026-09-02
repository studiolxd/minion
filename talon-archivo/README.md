# Set de comandos propio para Talon Voice

Sin `talonhub/community`. Solo acciones del core de Talon y acciones `user.*`
definidas en `lib/`.

- macOS · Talon 0.4.0 · motor Conformer (inglés)
- Uso previsto: **control del ordenador**, no dictado. El dictado en español
  va por Handy/Whisper, fuera de Talon.

## Estructura

```
~/.talon/user/mio/
├── core/
│   ├── engine.talon    # dormir/despertar el micro
│   ├── edit.talon      # portapapeles, deshacer, navegación de texto
│   └── window.talon    # ventanas, escritorios, sistema
├── apps/
│   ├── chrome.talon    # com.google.Chrome
│   ├── safari.talon    # com.apple.Safari
│   ├── terminal.talon  # com.apple.Terminal
│   └── orca.talon      # com.stablyai.orca
└── lib/
    ├── edit_mac.py            # IMPLEMENTA las acciones edit.* en macOS
    ├── switcher.py            # acción user.focus_app
    ├── switcher.talon         # comando "focus ..."
    └── launch_app.talon-list  # palabra hablada -> bundle id
```

`chrome.talon` y `safari.talon` tienen el mismo cuerpo a propósito: son dos
archivos en vez de uno con contextos combinados, para que un error en uno no
tumbe el otro. **Si cambias un comando de navegador, cámbialo en los dos.**

---

## Motor de reconocimiento (global) — `core/engine.talon`

| Frase | Efecto |
|---|---|
| `snooze now` | Apaga el micrófono |

**Rescate por teclado** (imprescindible: `snooze now` no se deshace por voz):

| Tecla | Efecto |
|---|---|
| `ctrl-espacio` | **Alterna la escucha.** Este es el que usas. |
| `ctrl-cmd-alt-s` | Lo mismo, red de seguridad por si otra app se queda con el anterior. |

`option-espacio` no se puede usar: lo tiene cogido Handy para el dictado.

`ctrl-espacio` choca con dos cosas del sistema. Si te dan problemas:
- **Cambiar fuente de entrada** — Ajustes → Teclado → Atajos de teclado →
  Fuentes de entrada. Desactívalo (inofensivo si solo tienes un idioma).
- **Autocompletar** en Xcode y en los JetBrains. Talon captura la tecla de
  forma global, así que en esas apps dejará de autocompletar.

## Edición (global) — `core/edit.talon`

| Frase | Efecto |
|---|---|
| `copy it` | Copiar |
| `paste it` | Pegar |
| `cut it` | Cortar |
| `save it` | Guardar |
| `undo it` | Deshacer |
| `redo it` | Rehacer |
| `select all` | Seleccionar todo |
| `scratch it` | Borrar un carácter |
| `delete word` | Borrar la palabra anterior |
| `word left` / `word right` | Cursor una palabra |
| `line home` / `line ending` | Inicio / fin de línea |
| `file topper` / `file bottom` | Inicio / fin del documento |
| `select word` / `select line` | Seleccionar palabra / línea |

## Ventanas y sistema (global) — `core/window.talon`

| Frase | Efecto |
|---|---|
| `window close` | `cmd-w` |
| `window mini` | Minimizar |
| `window full` | Pantalla completa |
| `window new` | `cmd-n` |
| `app hide` | Ocultar la app |
| `app switch` | `cmd-tab` |
| `desk left` / `desk right` | Escritorio anterior / siguiente |
| `mission view` | Mission Control |
| `spot light` | Spotlight |
| `shoot area` | Captura de zona |
| `shoot screen` | Captura de pantalla completa |

## Cambio de aplicación (global) — `lib/switcher.talon`

Enfoca la app si está abierta; si no, la lanza.

| Frase | App |
|---|---|
| `focus chrome` / `focus browser` | Google Chrome |
| `focus safari` | Safari |
| `focus orca` | Orca |
| `focus terminal` | Terminal |
| `focus talon` | Talon |

Para añadir una app: `defaults read "/Applications/Loquesea.app/Contents/Info" CFBundleIdentifier`
y añade una línea `palabra: bundle.id` a `lib/launch_app.talon-list`.

## Navegadores — `apps/chrome.talon`, `apps/safari.talon`

| Frase | Efecto |
|---|---|
| `tab new` / `tab close` / `tab restore` | Pestaña nueva / cerrar / reabrir |
| `tab next` / `tab previous` | Pestaña siguiente / anterior |
| `tab one` … `tab five` | Ir a la pestaña 1–5 |
| `tab final` | Última pestaña |
| `page back` / `page forward` | Atrás / adelante |
| `page reload` | Recargar |
| `page topper` / `page bottom` | Arriba / abajo del todo |
| `address bar` | Barra de direcciones |
| `find here` | Buscar en la página |
| `zoom more` / `zoom less` / `zoom reset` | Zoom |

## Terminal — `apps/terminal.talon`

| Frase | Efecto |
|---|---|
| `tab new` / `tab close` | Pestaña nueva / cerrar |
| `tab next` / `tab previous` | Cambiar de pestaña |
| `clear it` | `ctrl-l` |
| `cancel it` | `ctrl-c` |
| `history back` / `history next` | Historial arriba / abajo |
| `line home` / `line ending` | `ctrl-a` / `ctrl-e` |
| `kill line` | `ctrl-k` |

**Comandos escritos, sin pulsar Enter** — se quedan en el prompt para que los
revises antes de ejecutar:

| Frase | Escribe |
|---|---|
| `list files` | `ls -la` |
| `show status` | `git status` |
| `show branch` | `git branch` |
| `show diff` | `git diff` |
| `show log` | `git log --oneline -20` |
| `show docker` | `docker ps` |

## Orca — `apps/orca.talon`

⚠️ **Sin verificar.** Son los atajos convencionales de un editor tipo VS Code.
Comprueba cada uno dentro de Orca y corrige el que no coincida.

| Frase | Atajo supuesto |
|---|---|
| `palette open` | `cmd-shift-p` |
| `file open` | `cmd-p` |
| `side bar` | `cmd-b` |
| `panel toggle` | `cmd-j` |
| `comment line` | `cmd-/` |
| `find here` | `cmd-f` |
| `find project` | `cmd-shift-f` |

---

## Notas de mantenimiento

- **Colisiones intencionadas.** `tab close`, `tab next`, `find here` y
  `line home` existen en varios archivos, pero en contextos distintos
  (navegador vs. terminal). Gana siempre el contexto más específico. No
  añadas ninguna de esas frases a un archivo global.
- **La trampa grande: declarada != implementada.** El core de Talon
  *declara* `edit.copy`, `edit.select_all`, etc., pero las deja **vacías**.
  Aparecen en `dir(actions.edit)` y en el REPL como si existieran, y aun así
  fallan al ejecutarse:

  ```
  NotImplementedError: Action 'edit.select_all' exists but the Module
  method is empty and no Context reimplements it
  ```

  Quien las rellena normalmente es community. Aquí lo hace `lib/edit_mac.py`.
  **Comprobar que una acción existe no basta: hay que ejecutarla.**
- **Firmas.** Al implementar una acción hay que respetar su prototipo o Talon
  rechaza el archivo entero. Las de `edit` con parámetros son:
  `find(text=None)`, `select_line(n=None)`, `select_lines(a, b)`,
  `extend_line(n)`, `extend_column(n)`, `jump_line(n)`, `jump_column(n)`,
  `selected_text() -> str`. El resto no lleva argumentos.
- `edit.delete_word_left` **no existe** en el core (es de community); la
  correcta es `edit.delete_word`.
- **`user.focus_app` probada en los tres casos**: app cerrada (la lanza), app
  corriendo sin ventanas (le pide una) y app con ventana (solo enfoca). El
  segundo caso es el que no es obvio: en macOS cerrar la última ventana no
  cierra la app, y `app.focus()` entonces cambia la barra de menús sin mostrar
  nada. Se detecta con `app.windows()` y se resuelve con `ui.launch(bundle=)`,
  que equivale a pulsar el icono del Dock.
- **Ver el log:** menú de Talon → `Scripting` → `View Log`, o
  `tail -f ~/.talon/talon.log`. Un archivo con error de sintaxis se desactiva
  entero hasta que se arregle.
- **Bundle id de la app en primer plano:** menú → `Scripting` → `Open REPL`,
  luego `ui.active_app()`.
- **Teclado español ISO.** Es la causa de fallos silenciosos: el comando se
  reconoce, la tecla se envía y no pasa nada. Talon manda *posiciones* de
  tecla, no caracteres, así que todo atajo que dependa de un carácter que en
  ISO español no es tecla física (`[`, `]`, `=`) llega a otro sitio. Evita:

  | No uses | Usa | Para |
  |---|---|---|
  | `cmd-[` / `cmd-]` | `cmd-left` / `cmd-right` | Atrás / adelante |
  | `cmd-[` / `cmd-]` | `tab` / `shift-tab` | Indentar |
  | `cmd-=` / `cmd--` | `cmd-plus` / `cmd-minus` | Zoom |

  `plus` y `minus` sí se resuelven según el layout activo. Y ojo: Talon acepta
  `cmd-[` sin protestar — no hay error, simplemente no funciona.
- **Elección de palabras.** Todo comando es de dos palabras: más señal
  acústica, muchos menos falsos positivos que una palabra suelta. Evita
  añadir comandos de una sola sílaba o que aparezcan al hablar normal.

## Por qué no usamos talonhub/community

Community aporta ~1000 comandos: alfabeto fonético, formatters, dictado en
inglés, modos command/dictation. Casi todo su valor está en **dictar texto en
inglés**, que aquí no hace falta: el dictado va en español por Handy.

Lo único que sí necesitábamos de community era la capa que implementa las
acciones `edit.*` en macOS. Son las ~150 líneas de `lib/edit_mac.py`.

El coste de instalarlo sería un vocabulario de mil frases compitiendo con las
nuestras en un motor inglés escuchando a un hablante español: muchos más
falsos positivos, y colisiones difíciles de rastrear.

Sí merece la pena **leer** su código cuando falte implementar algo:
https://github.com/talonhub/community

## Pendiente (fase 2)

- `core/mouse.talon`: clic, doble clic, clic derecho, arrastre y scroll.
- Rejilla de pantalla para apuntar sin trackpad. Requiere un módulo con
  canvas propio en Python; es trabajo de su propia sesión.
