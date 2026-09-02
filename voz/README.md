# Comandos de voz en castellano (prototipo)

Puente entre **Handy** (que entiende español) y **acciones en macOS**.

## Cómo funciona

Handy pasa cada transcripción a `comandos.py` por `stdin`. El script decide:

| Dices | Qué pasa |
|---|---|
| «Ordenador, abre Chrome» | Enfoca Chrome. **No escribe nada.** |
| «abre Chrome a ver qué pasa» | Se escribe tal cual, como dictado normal |
| «Ordenador, haz un pino» | No lo entiende: suena un aviso y no escribe nada |

La regla: **si la frase empieza por "ordenador", es una orden.** En cualquier
otra posición no cuenta, así que puedes dictar "hablé con el ordenador sobre
Safari" sin que ejecute nada.

Handy pega lo que el script escriba en `stdout`. Si no escribe nada, no pega
nada — así se ejecuta un comando sin ensuciar el documento.

## Comandos actuales

Solo enfocar aplicaciones. 143 frases reconocidas, generadas combinando:

- **Verbos:** abre, ábreme, ve a, vete a, cambia a, enfoca, salta a, pon,
  ponme, dame, muestra, trae
- **Apps:** chrome (o "el navegador"), safari, terminal, orca, talon

También vale el nombre suelto: «Ordenador, Safari».

No hace falta que la frase esté en la lista: se acepta lo que se parezca
por encima del 78% (`UMBRAL`). Por eso «tráeme la terminal» funciona.

## Configuración de Handy (ya aplicada)

| Ajuste | Valor |
|---|---|
| `paste_method` | `external_script` |
| `external_script_path` | `/Users/suvi/Dev/talon/voz/comandos.py` |
| `selected_language` | `es` |
| `always_on_microphone` | `true` (solo baja la latencia; **no** es manos libres) |

**La opción "External Script" no aparece en la interfaz de macOS**: Handy la
documenta como "Linux only" y la oculta. Pero el backend sí la soporta —
funciona. Hubo que escribirla a mano en:

```
~/Library/Application Support/com.pais.handy/settings_store.json
```

Handy hay que cerrarlo antes de editar ese archivo, o lo sobrescribe al salir.
Hay una copia del estado previo en `settings_store.json.bak`.

**Cómo pasa Handy la transcripción:** por `stdin`. El script también acepta
argumento y variables de entorno, por si cambia entre versiones, y nunca se
queda bloqueado esperando (`select` con límite de 1 s).

## Probar sin hablar

```bash
./comandos.py --probar "ordenador abre chrome"
echo "Ordenador, vete a Safari." | ./comandos.py
```

## El registro es la herramienta de afinado

`voz.log` guarda cada frase y qué se hizo con ella:

```
COMANDO  'abre cromo' -> enfocar chrome
TEXTO    'Esto es dictado normal.'
NO ENTENDIDO  'haz un pino'
```

Cuando un comando no salga, mira ahí **qué transcribió Parakeet**. Casi
siempre la solución es añadir esa transcripción como alias, no tocar el
umbral. Ejemplo real: "Chrome" en español sale a menudo como "cromo", que ya
está contemplado.

## Decisiones de diseño

- **Sin dependencias.** Solo biblioteca estándar (`difflib` para el parecido).
  Se ejecuta con el Python del sistema, sin entorno virtual que mantener.
- **Nunca pierde dictado.** Si el script falla, devuelve el texto original en
  vez de nada. Como mucho escribe una orden en lugar de ejecutarla.
- **`open -b` para enfocar.** Cubre de una vez los tres casos: app cerrada
  (la lanza), corriendo sin ventanas (le pide una) y con ventana (la activa).
- **Sonidos como respuesta.** `Pop` al ejecutar, `Basso` si no entiende. Sin
  ellos no sabrías si te ha oído, porque un comando no escribe nada.

## Los dos modos

`ESCUCHA_CONTINUA` (arriba del script) cambia qué pasa con lo que **no** es un
comando:

| | `False` (ahora) | `True` |
|---|---|---|
| «Ordenador, abre Chrome» | ejecuta | ejecuta |
| «Escribe hola qué tal» | escribe todo, incluido "escribe" | escribe «hola qué tal» |
| «Mañana quedamos a las cinco» | lo escribe | **lo ignora** |

Está en `False` porque sigues en push-to-talk: pulsas para hablar, así que
todo lo que digas es intencionado y debe escribirse.

Ponlo en `True` **solo** si algún día el micrófono está de verdad siempre
abierto. Si no, dictar un correo sin decir "escribe" no escribiría nada.

## Estado

- **Talon desinstalado.** El set de 49 comandos en inglés quedó archivado en
  `../talon-archivo/` y la app está en la papelera, por si hiciera falta
  volver. Su README documenta las trampas que encontramos (acciones del core
  declaradas pero vacías, teclado ISO español, apps sin ventanas).

## Pendiente

- **Pulsar teclas** (copiar, guardar, pestañas). Necesita permiso de
  Accesibilidad para **Handy**, no solo para el script.
- **Contexto por aplicación**: que «cierra pestaña» haga cosas distintas según
  lo que esté delante. Se consulta con AppleScript.
- **Manos libres de verdad**: Handy no lo ofrece. Requiere un proceso propio
  que escuche en bucle (VAD para cortar por silencios + Parakeet por
  segmento). Ahí sí tocaría poner `ESCUCHA_CONTINUA = True`.
