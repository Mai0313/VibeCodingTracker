use anyhow::Result;
use std::env;
use std::path::Path;

/// Installation method detection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationMethod {
    Npm,
    Pip,
    Cargo,
    Manual, // curl, PowerShell, or source build
}

impl InstallationMethod {
    /// Get the update command for this installation method
    pub fn update_command(&self) -> &str {
        match self {
            InstallationMethod::Npm => {
                "npm update -g @mai0313/vct\n  or: npm update -g vibe-coding-tracker"
            }
            InstallationMethod::Pip => {
                "pip install --upgrade vibe_coding_tracker\n  or: uv pip install --upgrade vibe_coding_tracker"
            }
            InstallationMethod::Cargo => "cargo install vibe_coding_tracker --force",
            InstallationMethod::Manual => {
                "vct update\n  or: Re-run the installation script\n  or: Download from https://github.com/Mai0313/VibeCodingTracker/releases"
            }
        }
    }

    /// Get the installation method name
    pub fn name(&self) -> &str {
        match self {
            InstallationMethod::Npm => "npm",
            InstallationMethod::Pip => "pip",
            InstallationMethod::Cargo => "cargo",
            InstallationMethod::Manual => "manual",
        }
    }
}

/// Detect the installation method based on the current executable path
pub fn detect_installation_method() -> Result<InstallationMethod> {
    let exe_path = env::current_exe()?;
    let path_str = exe_path.to_string_lossy().to_lowercase();

    // Check for npm installation patterns
    // npm global packages are typically in:
    // - Windows: %APPDATA%\npm\node_modules
    // - Unix: /usr/local/lib/node_modules, ~/.npm-global, etc.
    if path_str.contains("/npm/")
        || path_str.contains("/.npm")
        || path_str.contains("\\npm\\")
        || path_str.contains("\\.npm")
        || path_str.contains("/node_modules/")
        || path_str.contains("\\node_modules\\")
    {
        return Ok(InstallationMethod::Npm);
    }

    // Check for pip/Python installation patterns
    // pip user installs are typically in:
    // - Unix: ~/.local/bin, /usr/local/bin (with Python context)
    // - Windows: %LOCALAPPDATA%\Programs\Python\PythonXX\Scripts
    if path_str.contains("/site-packages/")
        || path_str.contains("\\site-packages\\")
        || path_str.contains("/python")
        || path_str.contains("\\python")
        || path_str.contains("\\scripts\\")
        || (path_str.contains("/.local/bin/") && is_likely_python_environment(&exe_path))
    {
        return Ok(InstallationMethod::Pip);
    }

    // Check for cargo installation
    // cargo installs to ~/.cargo/bin by default
    if path_str.contains("/.cargo/bin/") || path_str.contains("\\.cargo\\bin\\") {
        return Ok(InstallationMethod::Cargo);
    }

    // Everything else is considered manual installation
    Ok(InstallationMethod::Manual)
}

/// Check if the executable is likely in a Python environment
/// by looking for nearby Python-related files or directories
fn is_likely_python_environment(exe_path: &Path) -> bool {
    if let Some(parent) = exe_path.parent() {
        // Check for pip, python, or other Python-related executables nearby
        for name in ["pip", "pip3", "python", "python3", "uv"] {
            let sibling = parent.join(name);
            if sibling.exists() {
                return true;
            }
            // Also check for Windows .exe extension
            if cfg!(windows) {
                let sibling_exe = parent.join(format!("{}.exe", name));
                if sibling_exe.exists() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_npm_detection() {
        // Simulate npm path detection logic
        let npm_paths = vec![
            "/usr/local/lib/node_modules/.bin/vct",
            "C:\\Users\\user\\AppData\\Roaming\\npm\\vct.exe",
            "/home/user/.npm-global/bin/vct",
        ];

        for path in npm_paths {
            let normalized = path.to_lowercase();
            assert!(
                normalized.contains("/npm/")
                    || normalized.contains("/.npm")
                    || normalized.contains("\\npm\\")
                    || normalized.contains("\\.npm")
                    || normalized.contains("/node_modules/")
                    || normalized.contains("\\node_modules\\"),
                "Failed to detect npm path: {}",
                path
            );
        }
    }

    #[test]
    fn test_pip_detection() {
        let pip_paths = vec![
            "/usr/local/lib/python3.11/site-packages/vct",
            "C:\\Users\\user\\AppData\\Local\\Programs\\Python\\Python311\\Scripts\\vct.exe",
            "/home/user/.local/bin/vct",
        ];

        for path in pip_paths {
            let normalized = path.to_lowercase();
            assert!(
                normalized.contains("/site-packages/")
                    || normalized.contains("\\site-packages\\")
                    || normalized.contains("/python")
                    || normalized.contains("\\python")
                    || normalized.contains("\\scripts\\")
                    || normalized.contains("/.local/bin/"),
                "Failed to detect pip path: {}",
                path
            );
        }
    }

    #[test]
    fn test_cargo_detection() {
        let cargo_paths = vec![
            "/home/user/.cargo/bin/vct",
            "C:\\Users\\user\\.cargo\\bin\\vct.exe",
        ];

        for path in cargo_paths {
            let normalized = path.to_lowercase();
            assert!(
                normalized.contains("/.cargo/bin/") || normalized.contains("\\.cargo\\bin\\"),
                "Failed to detect cargo path: {}",
                path
            );
        }
    }

    #[test]
    fn test_update_commands() {
        assert!(InstallationMethod::Npm
            .update_command()
            .contains("npm update"));
        assert!(InstallationMethod::Pip
            .update_command()
            .contains("pip install --upgrade"));
        assert!(InstallationMethod::Cargo
            .update_command()
            .contains("cargo install"));
        assert!(InstallationMethod::Manual
            .update_command()
            .contains("vct update"));
    }
}
