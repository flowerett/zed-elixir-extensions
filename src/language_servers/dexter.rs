use std::fs;

use zed_extension_api::{
    self as zed, CodeLabel, CodeLabelSpan, LanguageServerId, Result, Worktree,
    lsp::{Completion, CompletionKind, Symbol, SymbolKind},
    serde_json::{Value, json},
};

use crate::language_servers::{config, util};

struct DexterBinary {
    path: String,
    args: Vec<String>,
}

pub struct Dexter {
    cached_binary_path: Option<String>,
}

impl Dexter {
    pub const LANGUAGE_SERVER_ID: &'static str = "dexter";

    pub fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    pub fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let dexter = self.language_server_binary(language_server_id, worktree)?;

        Ok(zed::Command {
            command: dexter.path,
            args: dexter.args,
            env: Default::default(),
        })
    }

    fn language_server_binary(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<DexterBinary> {
        let (platform, arch) = zed::current_platform();

        let archive_name = format!(
            "{}_{os}_{arch}",
            Self::LANGUAGE_SERVER_ID,
            os = match platform {
                zed::Os::Mac => "Darwin",
                zed::Os::Linux => "Linux",
                zed::Os::Windows => return Err(format!("unsupported platform: {platform:?}")),
            },
            arch = match arch {
                zed::Architecture::Aarch64 => "arm64",
                zed::Architecture::X8664 => "x86_64",
                zed::Architecture::X86 =>
                    return Err(format!("unsupported architecture: {arch:?}")),
            },
        );

        let binary_name = format!("{}/{}", archive_name, Self::LANGUAGE_SERVER_ID);
        let binary_settings = config::get_binary_settings(Self::LANGUAGE_SERVER_ID, worktree);
        let binary_args =
            config::get_binary_args(&binary_settings).unwrap_or_else(|| vec!["lsp".to_string()]);

        if let Some(binary_path) = config::get_binary_path(&binary_settings) {
            return Ok(DexterBinary {
                path: binary_path,
                args: binary_args,
            });
        }

        if let Some(binary_path) = worktree.which(Self::LANGUAGE_SERVER_ID) {
            return Ok(DexterBinary {
                path: binary_path,
                args: binary_args,
            });
        }

        if let Some(binary_path) = &self.cached_binary_path
            && fs::metadata(binary_path).is_ok_and(|stat| stat.is_file())
        {
            return Ok(DexterBinary {
                path: binary_path.clone(),
                args: binary_args,
            });
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = match zed::latest_github_release(
            "remoteoss/dexter",
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        ) {
            Ok(release) => release,
            Err(_) => {
                if let Some(binary_path) =
                    util::find_existing_binary(Self::LANGUAGE_SERVER_ID, &binary_name)
                {
                    self.cached_binary_path = Some(binary_path.clone());
                    return Ok(DexterBinary {
                        path: binary_path,
                        args: binary_args,
                    });
                }
                return Err("failed to download latest github release".to_string());
            }
        };

        let asset_name = format!("{archive_name}.tar.gz");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {:?}", asset_name))?;

        let version_dir = format!("{}-{}", Self::LANGUAGE_SERVER_ID, release.version);
        fs::create_dir_all(&version_dir).map_err(|e| format!("failed to create directory: {e}"))?;

        let binary_path = format!("{}/{}", version_dir, binary_name);

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::GzipTar,
            )
            .map_err(|e| format!("failed to download file: {e}"))?;

            zed::make_file_executable(&binary_path)?;

            util::remove_outdated_versions(Self::LANGUAGE_SERVER_ID, &version_dir)?;
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(DexterBinary {
            path: binary_path,
            args: binary_args,
        })
    }

    pub fn language_server_initialization_options(
        &mut self,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        let settings = config::get_initialization_options(Self::LANGUAGE_SERVER_ID, worktree)
            .unwrap_or_else(|| {
                json!({
                    "followDelegates": true
                })
            });

        Ok(Some(settings))
    }

    pub fn language_server_workspace_configuration(
        &mut self,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        let settings = config::get_workspace_configuration(Self::LANGUAGE_SERVER_ID, worktree)
            .unwrap_or_default();

        Ok(Some(settings))
    }

    pub fn label_for_completion(&self, completion: Completion) -> Option<CodeLabel> {
        match completion.kind? {
            CompletionKind::Module | CompletionKind::Class => {
                let name = completion.label;
                let defmodule = "defmodule ";
                let code = format!("{defmodule}{name}");

                Some(CodeLabel {
                    code,
                    spans: vec![CodeLabelSpan::code_range(
                        defmodule.len()..defmodule.len() + name.len(),
                    )],
                    filter_range: (0..name.len()).into(),
                })
            }
            CompletionKind::Function | CompletionKind::Constant => {
                let name = completion.label;
                let def = "def ";
                let code = format!("{def}{name}");

                Some(CodeLabel {
                    code,
                    spans: vec![CodeLabelSpan::code_range(def.len()..def.len() + name.len())],
                    filter_range: (0..name.len()).into(),
                })
            }
            _ => None,
        }
    }

    pub fn label_for_symbol(&self, symbol: Symbol) -> Option<CodeLabel> {
        let name = &symbol.name;

        let (code, filter_range, display_range) = match symbol.kind {
            SymbolKind::Module | SymbolKind::Interface | SymbolKind::Struct => {
                let defmodule = "defmodule ";
                let code = format!("{defmodule}{name}");
                let filter_range = 0..name.len();
                let display_range = defmodule.len()..defmodule.len() + name.len();
                (code, filter_range, display_range)
            }
            SymbolKind::Function | SymbolKind::Constant => {
                let def = "def ";
                let code = format!("{def}{name}");
                let filter_range = 0..name.len();
                let display_range = def.len()..def.len() + name.len();
                (code, filter_range, display_range)
            }
            _ => return None,
        };

        Some(CodeLabel {
            spans: vec![CodeLabelSpan::code_range(display_range)],
            filter_range: filter_range.into(),
            code,
        })
    }
}
