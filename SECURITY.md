# Seguridad

Karbyx ejecuta comandos y edita archivos a petición de un modelo. Por eso nos
tomamos en serio cualquier forma de saltarse sus permisos.

## Avisar de un problema

**No abras un issue.** Usa el [aviso privado de
GitHub](https://github.com/CurroRaya/karbyx/security/advisories/new) (pestaña
*Security* › *Report a vulnerability*) y cuenta:

- Qué se puede conseguir y en qué modo (Pregunta antes, Automático o Total).
- Los pasos, y si hace falta, el archivo, la página o la respuesta del modelo que
  lo provoca. Sin claves de verdad.
- La versión de Karbyx (o el commit) y la de macOS.

Karbyx lo mantiene una persona en su tiempo libre: no hay plazos garantizados ni
recompensas. Te contestaremos en cuanto podamos y, si lo quieres, aparecerás en
los agradecimientos del arreglo.

## Versiones con soporte

Solo la última versión y la rama `main`.

## Qué es un fallo de seguridad

Por ejemplo, que en *Pregunta antes* o en *Automático*, sin que lo apruebes:

- Un comando salga del sandbox, use la red, escriba fuera de la carpeta o lea las
  claves que el sandbox protege.
- Se edite fuera de la carpeta del proyecto o en los archivos protegidos (`.git`,
  `.karbyx`, `.claude`...).
- Se arranque un servidor MCP, se use una de sus herramientas o se guarde algo en
  la memoria.
- Una tarjeta de permiso enseñe algo distinto de lo que se hará.

Y, en cualquier modo: que una clave o una sesión llegue a quien no debe, o que una
página, un archivo o una habilidad ejecute algo al abrirlo o instalarlo.

## Qué no lo es

Ya está avisado en [Qué protege y qué no](README.md#qué-protege-y-qué-no):

- Lo que haga el modelo en modo **Total**, que no pregunta nada ni usa el sandbox,
  o un comando que hayas permitido *Fuera del sandbox*.
- Que la conexión con el servidor de modelos sea HTTP sin cifrar: es para tu red.
- Que un comando del sandbox lea archivos que no están protegidos, como el `.env`
  del proyecto.
- Lo que hagan un servidor MCP o un asistente externo que tú hayas aprobado.
