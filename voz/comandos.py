#!/usr/bin/env python3
"""Puente Handy -> comandos de voz en castellano.

Handy pasa la transcripcion por stdin. Si este script termina con codigo 0
y escribe algo en stdout, Handy pega ese texto; si no escribe nada, no pega
nada. Eso permite:

  "ordenador, abre Chrome"  -> ejecuta el comando, no escribe nada
  "abre Chrome a ver"       -> no lleva palabra clave: se escribe tal cual

Sin dependencias externas: solo biblioteca estandar.

Para probarlo sin hablar:
    ./comandos.py --probar "ordenador abre chrome"
"""

import difflib
import os
import select
import subprocess
import sys
import unicodedata
from datetime import datetime
from pathlib import Path

# --- Configuracion --------------------------------------------------------

# Palabra(s) que marcan que la frase es una orden y no dictado.
PALABRAS_CLAVE = ("ordenador", "ordenadora", "computador")

# Con el microfono siempre abierto, TODO lo que se hable llega aqui. Si en ese
# modo pegasemos lo que no es comando, se escribiria en pantalla cada
# conversacion. Asi que con escucha continua hay que pedir el dictado tambien:
#
#   "ordenador, abre Chrome"     -> ejecuta
#   "escribe hola que tal"       -> escribe "hola que tal"
#   cualquier otra cosa          -> se ignora
#
# Con push-to-talk (False) se mantiene lo de antes: lo que no es comando se
# escribe, porque has pulsado una tecla para decirlo.
# Ponlo en True SOLO cuando el microfono este realmente siempre abierto.
# Con push-to-talk debe ser False: si no, dictar un correo sin decir
# "escribe" no escribiria nada.
ESCUCHA_CONTINUA = False

# Palabras que piden explicitamente escribir lo que viene detras.
PALABRAS_DICTADO = ("escribe", "escriba", "dicta", "anota", "apunta")

# Umbral de parecido (0-1) para aceptar una frase como comando.
# Por debajo, se considera que no hemos entendido y no se ejecuta nada.
UMBRAL = 0.78

REGISTRO = Path(__file__).parent / "voz.log"

# Aplicaciones que se pueden enfocar, con sus alias hablados.
APPS = {
    "com.google.Chrome": ["chrome", "crome", "el navegador", "navegador"],
    "com.apple.Safari": ["safari"],
    "com.apple.Terminal": ["terminal", "la terminal", "el terminal"],
    "com.stablyai.orca": ["orca", "orka"],
    "com.talonvoice.Talon": ["talon"],
}

# Verbos que valen para "traeme esta app".
VERBOS_FOCO = ["abre", "abreme", "ve a", "vete a", "cambia a", "enfoca",
               "salta a", "pon", "ponme", "dame", "muestra", "trae"]

SONIDOS = {
    "ok": "/System/Library/Sounds/Pop.aiff",
    "error": "/System/Library/Sounds/Basso.aiff",
}


# --- Utilidades -----------------------------------------------------------

def normalizar(texto: str) -> str:
    """Minusculas, sin tildes, sin puntuacion, sin espacios de sobra.

    Parakeet devuelve texto con puntuacion y mayusculas ("Ordenador, abre
    Chrome."), asi que hay que limpiarlo antes de comparar.
    """
    texto = unicodedata.normalize("NFD", texto.lower())
    texto = "".join(c for c in texto if unicodedata.category(c) != "Mn")
    texto = "".join(c if c.isalnum() or c.isspace() else " " for c in texto)
    return " ".join(texto.split())


def registrar(linea: str) -> None:
    """Deja constancia de lo que se oyo y que se hizo. Sirve para afinar."""
    try:
        marca = datetime.now().strftime("%H:%M:%S")
        with REGISTRO.open("a", encoding="utf-8") as f:
            f.write(f"{marca}  {linea}\n")
    except OSError:
        pass  # el registro nunca debe impedir que funcione el dictado


def sonar(cual: str) -> None:
    ruta = SONIDOS.get(cual)
    if ruta:
        subprocess.Popen(["/usr/bin/afplay", ruta],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# --- Acciones -------------------------------------------------------------

def enfocar(bundle: str) -> None:
    """Trae la app al frente.

    'open -b' cubre los tres casos de una vez: la lanza si esta cerrada, le
    pide una ventana si corre sin ninguna, y la activa si ya la tiene.
    """
    subprocess.run(["/usr/bin/open", "-b", bundle], check=False)


# --- Tabla de comandos ----------------------------------------------------

def construir_tabla():
    """Genera {frase normalizada: (descripcion, funcion)}.

    Se generan todas las combinaciones de verbo + alias, mas el alias suelto
    ("ordenador, chrome"), que es como se acaba hablando en la practica.
    """
    tabla = {}
    for bundle, alias in APPS.items():
        nombre = alias[0]
        for a in alias:
            accion = (f"enfocar {nombre}", lambda b=bundle: enfocar(b))
            tabla[normalizar(a)] = accion
            for verbo in VERBOS_FOCO:
                tabla[normalizar(f"{verbo} {a}")] = accion
    return tabla


TABLA = construir_tabla()


# --- Interpretacion -------------------------------------------------------

def quitar_prefijo(texto: str, prefijos) -> str:
    """Devuelve el resto de la frase si empieza por uno de los prefijos."""
    palabras = texto.split()
    if palabras and palabras[0] in prefijos:
        return " ".join(palabras[1:])
    return None


def buscar_comando(frase: str):
    """Busca el comando mas parecido. Devuelve (descripcion, funcion) o None."""
    if not frase:
        return None
    if frase in TABLA:
        return TABLA[frase]
    parecidas = difflib.get_close_matches(frase, TABLA.keys(), n=1,
                                          cutoff=UMBRAL)
    return TABLA[parecidas[0]] if parecidas else None


def procesar(texto_bruto: str) -> str:
    """Devuelve lo que hay que pegar (cadena vacia = no pegar nada)."""
    if not texto_bruto:
        return ""

    normal = normalizar(texto_bruto)
    resto = quitar_prefijo(normal, PALABRAS_CLAVE)

    if resto is None:
        if not ESCUCHA_CONTINUA:
            registrar(f"TEXTO   {texto_bruto!r}")
            return texto_bruto

        # Escucha continua: solo escribimos si se ha pedido escribir.
        dictado = quitar_prefijo(normal, PALABRAS_DICTADO)
        if dictado:
            # Se devuelve recortando el prefijo del texto original, para no
            # perder tildes ni mayusculas al escribir.
            corte = len(texto_bruto.split()[0])
            texto = texto_bruto[corte:].lstrip(" ,.:;")
            registrar(f"DICTADO  {texto!r}")
            return texto

        registrar(f"IGNORADO  {texto_bruto!r}")
        return ""

    comando = buscar_comando(resto)
    if comando is None:
        registrar(f"NO ENTENDIDO  {resto!r}  (de {texto_bruto!r})")
        sonar("error")
        return ""

    descripcion, funcion = comando
    registrar(f"COMANDO  {resto!r} -> {descripcion}")
    funcion()
    sonar("ok")
    return ""


def leer_entrada() -> str:
    """Obtiene la transcripcion, venga por donde venga.

    Handy no documenta el contrato en macOS: puede pasarla por stdin, como
    argumento o en una variable de entorno. Probamos las tres, y nunca nos
    quedamos bloqueados esperando una entrada que no va a llegar.
    """
    # 1) Argumento: es lo mas explicito, tiene prioridad.
    if len(sys.argv) > 1 and sys.argv[1].strip():
        return " ".join(sys.argv[1:]).strip()

    # 2) stdin, pero con limite de espera: si nadie escribe, seguimos.
    #    Sin esto, una llamada sin datos dejaria el proceso colgado.
    try:
        if not sys.stdin.isatty():
            listo, _, _ = select.select([sys.stdin], [], [], 1.0)
            if listo:
                texto = sys.stdin.read().strip()
                if texto:
                    return texto
    except Exception:
        pass

    # 3) Variables de entorno.
    for var in ("HANDY_TRANSCRIPT", "HANDY_TEXT", "TRANSCRIPT", "TEXT"):
        valor = os.environ.get(var, "").strip()
        if valor:
            return valor

    entorno = {k: v for k, v in os.environ.items()
               if "HANDY" in k.upper() or "TRANS" in k.upper()}
    registrar(f"SIN ENTRADA  argv={sys.argv[1:]!r}  entorno={entorno!r}")
    return ""


def main() -> int:
    if len(sys.argv) > 2 and sys.argv[1] == "--probar":
        entrada = sys.argv[2]
    else:
        entrada = leer_entrada()

    entrada = entrada.strip()

    # Red de seguridad: si algo falla aqui dentro, Handy no pegaria nada y
    # se perderia lo dictado. Ante cualquier error, devolvemos el texto tal
    # cual: como maximo se escribe una orden en vez de ejecutarla.
    try:
        salida = procesar(entrada)
    except Exception as e:  # noqa: BLE001
        registrar(f"ERROR  {type(e).__name__}: {e}  (de {entrada!r})")
        salida = entrada

    if salida:
        sys.stdout.write(salida)
    return 0


if __name__ == "__main__":
    sys.exit(main())
