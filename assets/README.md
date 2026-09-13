# Icono de Nanofy: Resonancia

Diseño seleccionado: aros entrelazados verde salvia sobre fondo carbón.
La propuesta original se creó con el generador de imágenes integrado y se conserva
en `resonancia-original.png`. Se recorta el margen de presentación al empaquetarla,
sin redibujar el símbolo.

- `nanofy.png`: imagen de 256 × 256.
- `nanofy.ico`: recurso de Windows con tamaños 16, 24, 32, 48, 64, 128 y 256.
- `nanofy-32.rgba`: píxeles integrados en la ventana; no requiere archivos externos
  ni decodificación de imagen al iniciar.
- `prepare-icon.cjs`: conversión reproducible con Node y sharp. Ejecutar
  `node assets/prepare-icon.cjs`; `NANOFY_SHARP` permite indicar la ruta de sharp.

`build.rs` incorpora el ICO al ejecutable de Windows. Los archivos generados se
incluyen en el proyecto; Node no es necesario para compilar o ejecutar Nanofy.
