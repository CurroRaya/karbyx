# Cómo contribuir a forge

¡Gracias por querer mejorar forge! Cualquier ayuda cuenta: informar de un
fallo, proponer una idea, mejorar la documentación o enviar código.

## Informar de un fallo o proponer una idea

Abre un *issue* en GitHub. Para los fallos, incluye:

- Versión de macOS y cómo lanzas forge.
- Pasos para reproducirlo y qué esperabas que pasara.
- Si es del chat: qué servidor y modelo usas, y su contexto. No pegues tu clave.
- Si es de un servidor MCP o una habilidad: cuál es y el error que enseña su panel.
  Quita antes las claves.
- Si es de rendimiento, la salida de `FORGE_PROFILE=1 forge-app`.

## Enviar cambios

1. Haz un *fork* del repositorio y crea una rama para tu cambio.
2. Asegúrate de que todo pasa antes de abrir el *pull request*:

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets   # sin avisos
   cargo test --workspace
   ```

3. Si tocas la interfaz, adjunta una captura. Puedes generarla sin permisos
   especiales con `FORGE_SCREENSHOT=/tmp/forge.png cargo run --release -p forge-app`
   (en el README hay más ayudas de desarrollo).
4. Abre el *pull request* explicando qué cambia y por qué.

### Convenciones

- Comentarios, mensajes de commit y textos de la interfaz en **español**.
- Cada archivo `.rs` empieza con la cabecera de licencia:

  ```rust
  // SPDX-License-Identifier: GPL-3.0-or-later
  // Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge
  ```

- Los cambios de comportamiento llevan tests; si algo no se puede testear
  automáticamente, explica en el PR cómo lo has probado.

## Licencia de las contribuciones

forge se publica bajo la licencia **GPL-3.0 o posterior**. Al enviar una
contribución aceptas que se distribuya bajo esa misma licencia. Así, todas las
mejoras siguen siendo libres para todo el mundo: nadie puede publicar una
versión modificada de forge sin compartir también su código.
