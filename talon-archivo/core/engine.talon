-
# --- Control del motor de reconocimiento ---
# CUIDADO: speech.disable() apaga el micro. No se puede volver por voz.
# El rescate es por teclado. Aprendete el atajo.

snooze now: speech.disable()

# Rescate principal: control + espacio.
# alt-space no sirve aqui: lo tiene cogido Handy para el dictado.
key(ctrl-space): speech.toggle()

# Red de seguridad, por si otra app se queda con ctrl-space.
key(ctrl-cmd-alt-s): speech.toggle()
