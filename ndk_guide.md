# Guía para Configurar cargo-ndk en macOS

## 1. Instalar Herramientas Necesarias

```bash
# Instalar Rust (si no lo tienes)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Instalar cargo-ndk
cargo install cargo-ndk
```

## 2. Instalar Android Studio y NDK

1. Descarga e instala Android Studio desde [https://developer.android.com/studio](https://developer.android.com/studio)
2. Abre Android Studio
3. Ve a Tools > SDK Manager
4. Selecciona la pestaña "SDK Tools"
5. Marca la casilla "NDK (Side by side)"
6. Haz clic en "Apply" y espera a que se instale

## 3. Configurar el NDK

1. Abre la Terminal
2. Verifica las versiones de NDK instaladas:
```bash
ls ~/Library/Android/sdk/ndk
```

3. Configura la variable de entorno (copia y pega exactamente):
```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"
```

4. Hazlo permanente (elige UNO según tu terminal):

Para zsh (terminal por defecto en Mac moderno):
```bash
echo 'export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"' >> ~/.zshrc
source ~/.zshrc
```

Para bash (terminal más antigua):
```bash
echo 'export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"' >> ~/.bash_profile
source ~/.bash_profile
```

5. Verifica que funcionó:
```bash
echo $ANDROID_NDK_HOME
```
Deberías ver la ruta completa del NDK.

## 4. Compilar tu Proyecto

1. Ve a la carpeta de tu proyecto:
```bash
cd ruta/de/tu/proyecto
```

2. Ejecuta:
```bash
cargo ndk -t armeabi-v7a build --release
```

## Solución de Problemas

Si obtienes algún error:

1. Verifica que la variable de entorno está configurada:
```bash
echo $ANDROID_NDK_HOME
```

2. Si no ves nada, repite el paso 3.

3. Prueba con una versión más reciente del NDK cambiando "25.1.8937393" por "28.0.13004108" en los comandos anteriores.

4. Asegúrate de que tu proyecto tiene el archivo `Android.mk` en la carpeta correcta.

## Notas Importantes

- No cierres la terminal después de exportar la variable ANDROID_NDK_HOME
- Si cierras la terminal, la configuración permanente (paso 4) debería mantener la variable configurada
- Si nada funciona, reinicia tu computadora y prueba de nuevo
