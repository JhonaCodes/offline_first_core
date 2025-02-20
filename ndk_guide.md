# Guide to Set Up `cargo-ndk` on macOS

## 1. Install Necessary Tools

```bash
# Install Rust (if you don't have it)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install cargo-ndk
cargo install cargo-ndk
```

## 2. Install Android Studio and NDK

1. Download and install Android Studio from [https://developer.android.com/studio](https://developer.android.com/studio)
2. Open Android Studio
3. Go to Tools > SDK Manager
4. Select the "SDK Tools" tab
5. Check the "NDK (Side by side)" box
6. Click "Apply" and wait for the installation to complete

## 3. Configure the NDK

1. Open Terminal
2. Check which NDK versions are installed:
```bash
ls ~/Library/Android/sdk/ndk
```

3. Set the environment variable (copy and paste exactly):
```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"
```

4. Make it permanent (choose ONE depending on your terminal):

For zsh (default terminal on modern Macs):
```bash
echo 'export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"' >> ~/.zshrc
source ~/.zshrc
```

For bash (older terminal):
```bash
echo 'export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/25.1.8937393"' >> ~/.bash_profile
source ~/.bash_profile
```

5. Verify that it worked:
```bash
echo $ANDROID_NDK_HOME
```
You should see the full path of the NDK.

## 4. Build Your Project

1. Go to your project folder:
```bash
cd path/to/your/project
```

2. Run:
```bash
cargo ndk -t armeabi-v7a build --release
```

## Troubleshooting

If you encounter any errors:

1. Check that the environment variable is set:
```bash
echo $ANDROID_NDK_HOME
```

2. If nothing shows up, repeat step 3.

3. Try using a newer version of the NDK by changing "25.1.8937393" to "28.0.13004108" in the previous commands.

4. Make sure your project has the `Android.mk` file in the correct folder.

## Important Notes

- Do not close the terminal after exporting the `ANDROID_NDK_HOME` variable.
- If you close the terminal, the permanent configuration (step 4) should keep the variable set.
- If nothing works, restart your computer and try again.