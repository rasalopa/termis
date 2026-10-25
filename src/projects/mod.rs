//! The project of a session: what it is built with, read from its
//! manifest, outside a repository the files of its directory, and a
//! file read for the viewer.

use std::path::Path;

pub mod read;
pub mod walk;

/// What a project is built with, as its manifest says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// The language the manifest implies.
    pub language: &'static str,
    /// A framework or library that marks the project, when a known one
    /// is among its dependencies.
    pub framework: Option<Framework>,
}

/// A dependency that marks a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framework {
    /// The name shown; the package's own when it is short.
    pub name: &'static str,
    /// The version the manifest asks for, digits and dots only, when it
    /// asks for one: `0.30.1` from `^0.30.1-beta`, none for a path, a
    /// URL, an alias or a tag.
    pub version: Option<String>,
}

/// A manifest file and how to read it.
type Reader = fn(&Path, &str) -> Stack;

/// The manifests looked for, in the order that decides when a project
/// has several; `package.json` last, since the other stacks keep one
/// for their assets.
const MANIFESTS: [(&str, Reader); 5] = [
    ("Cargo.toml", cargo),
    ("pyproject.toml", pyproject),
    ("go.mod", go_mod),
    ("composer.json", composer),
    ("package.json", package_json),
];

/// Crates that mark a Rust project, in the order that decides.
const RUST: [&str; 10] = [
    "ratatui",
    "axum",
    "actix-web",
    "rocket",
    "bevy",
    "tauri",
    "leptos",
    "dioxus",
    "yew",
    "tokio",
];
/// Packages that mark a JavaScript or TypeScript project, in the order
/// that decides, with the name shown.
const NODE: [(&str, &str); 12] = [
    ("next", "next"),
    ("nuxt", "nuxt"),
    ("@angular/core", "angular"),
    ("@nestjs/core", "nest"),
    ("astro", "astro"),
    ("@remix-run/react", "remix"),
    ("svelte", "svelte"),
    ("vue", "vue"),
    ("react", "react"),
    ("express", "express"),
    ("fastify", "fastify"),
    ("hono", "hono"),
];
/// Distributions that mark a Python project, in the order that decides.
const PYTHON: [&str; 6] = ["django", "fastapi", "flask", "streamlit", "typer", "click"];
/// Modules that mark a Go project, in the order that decides, with the
/// name shown; a major version suffix on the path counts too.
const GO: [(&str, &str); 6] = [
    ("github.com/gin-gonic/gin", "gin"),
    ("github.com/labstack/echo", "echo"),
    ("github.com/gofiber/fiber", "fiber"),
    ("github.com/go-chi/chi", "chi"),
    ("github.com/spf13/cobra", "cobra"),
    ("google.golang.org/grpc", "grpc"),
];
/// Packages that mark a PHP project, in the order that decides, with
/// the name shown.
const PHP: [(&str, &str); 5] = [
    ("laravel/framework", "laravel"),
    ("symfony/framework-bundle", "symfony"),
    ("symfony/symfony", "symfony"),
    ("slim/slim", "slim"),
    ("laminas/laminas-mvc", "laminas"),
];

/// The stack of the project at `root`, from the first of the manifests
/// found there; none without one. A manifest that cannot be read as
/// its format still names its language. A byte order mark is skipped.
pub fn detect(root: &Path) -> Option<Stack> {
    MANIFESTS.iter().find_map(|(name, read)| {
        let text = std::fs::read_to_string(root.join(name)).ok()?;
        Some(read(root, text.strip_prefix('\u{feff}').unwrap_or(&text)))
    })
}

/// Rust: the first of [`RUST`] among `[dependencies]` or, in a workspace
/// root, `[workspace.dependencies]`; a package entry without a version,
/// such as `{ workspace = true }`, takes the workspace's.
fn cargo(_: &Path, text: &str) -> Stack {
    let language = "Rust";
    let Ok(table) = text.parse::<toml::Table>() else {
        return Stack {
            language,
            framework: None,
        };
    };
    let sections = [
        table.get("dependencies"),
        table.get("workspace").and_then(|w| w.get("dependencies")),
    ];
    let framework = RUST.iter().find_map(|name| {
        let mut entries = sections
            .iter()
            .filter_map(|section| section.and_then(toml::Value::as_table))
            .filter_map(|deps| deps.get(*name))
            .peekable();
        entries.peek()?;
        Some(Framework {
            name,
            version: entries.find_map(toml_version),
        })
    });
    Stack {
        language,
        framework,
    }
}

/// JavaScript, or TypeScript when `typescript` is a dependency or a
/// `tsconfig.json` sits next to the manifest: the first of [`NODE`]
/// among `dependencies` and `devDependencies`.
fn package_json(root: &Path, text: &str) -> Stack {
    let configured = root.join("tsconfig.json").is_file();
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Stack {
            language: if configured {
                "TypeScript"
            } else {
                "JavaScript"
            },
            framework: None,
        };
    };
    let sections: Vec<&serde_json::Map<String, serde_json::Value>> =
        ["dependencies", "devDependencies"]
            .iter()
            .filter_map(|key| json.get(key).and_then(serde_json::Value::as_object))
            .collect();
    let dependency = |package: &str| sections.iter().find_map(|deps| deps.get(package));
    let language = if configured || dependency("typescript").is_some() {
        "TypeScript"
    } else {
        "JavaScript"
    };
    let framework = NODE.iter().find_map(|(package, name)| {
        let spec = dependency(package)?;
        Some(Framework {
            name,
            version: spec.as_str().and_then(version_of),
        })
    });
    Stack {
        language,
        framework,
    }
}

/// Python: the first of [`PYTHON`] among `[project].dependencies` (PEP
/// 508 requirements) or `[tool.poetry.dependencies]`.
fn pyproject(_: &Path, text: &str) -> Stack {
    let language = "Python";
    let Ok(table) = text.parse::<toml::Table>() else {
        return Stack {
            language,
            framework: None,
        };
    };
    let requirements: Vec<&str> = table
        .get("project")
        .and_then(|project| project.get("dependencies"))
        .and_then(toml::Value::as_array)
        .map(|list| list.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    let poetry = table
        .get("tool")
        .and_then(|tool| tool.get("poetry"))
        .and_then(|poetry| poetry.get("dependencies"))
        .and_then(toml::Value::as_table);
    let framework = PYTHON.iter().find_map(|name| {
        if let Some(spec) = requirements
            .iter()
            .find(|spec| normalized(requirement_name(spec)) == *name)
        {
            let rest = &spec.trim_start()[requirement_name(spec).len()..];
            return Some(Framework {
                name,
                version: requirement_version(rest),
            });
        }
        let (_, dependency) = poetry?.iter().find(|(key, _)| normalized(key) == *name)?;
        Some(Framework {
            name,
            version: toml_version(dependency),
        })
    });
    Stack {
        language,
        framework,
    }
}

/// Go: the first of [`GO`] among the `require` directives, those marked
/// `// indirect` left out.
fn go_mod(_: &Path, text: &str) -> Stack {
    let mut requirements: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let indirect = line.contains("// indirect");
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let requirement = if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            line
        } else if let Some(rest) = line.strip_prefix("require")
            && rest.starts_with(char::is_whitespace)
        {
            let rest = rest.trim();
            if rest == "(" {
                in_block = true;
                continue;
            }
            rest
        } else {
            continue;
        };
        if !indirect {
            requirements.push(requirement);
        }
    }
    let framework = GO.iter().find_map(|(module, name)| {
        requirements.iter().find_map(|requirement| {
            let mut parts = requirement.split_whitespace();
            let path = parts.next()?;
            let major = path.strip_prefix(module).is_some_and(|rest| {
                rest.is_empty()
                    || rest
                        .strip_prefix("/v")
                        .is_some_and(|v| v.bytes().all(|b| b.is_ascii_digit()))
            });
            major.then(|| Framework {
                name,
                version: parts.next().and_then(version_of),
            })
        })
    });
    Stack {
        language: "Go",
        framework,
    }
}

/// PHP: the first of [`PHP`] among `require`.
fn composer(_: &Path, text: &str) -> Stack {
    let language = "PHP";
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Stack {
            language,
            framework: None,
        };
    };
    let require = json.get("require").and_then(serde_json::Value::as_object);
    let framework = PHP.iter().find_map(|(package, name)| {
        let spec = require?.get(*package)?;
        Some(Framework {
            name,
            version: spec.as_str().and_then(version_of),
        })
    });
    Stack {
        language,
        framework,
    }
}

/// The version a TOML dependency asks for: the string itself, or the
/// `version` of a table.
fn toml_version(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(spec) => version_of(spec),
        toml::Value::Table(table) => table
            .get("version")
            .and_then(toml::Value::as_str)
            .and_then(version_of),
        _ => None,
    }
}

/// The version `spec` asks for when it is a version or a range of them,
/// starting with a number, an operator or `v`: the first run of digits
/// and dots, trailing dots dropped, `15.2` from `>=15.2,<16`. None from
/// `*`, a tag such as `latest`, or anything with a scheme, a path, a
/// fragment or an `@`: a URL, a path, an alias.
fn version_of(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.contains([':', '/', '#', '@'])
        || !spec.starts_with(|c: char| {
            c.is_ascii_digit() || matches!(c, '^' | '~' | '<' | '>' | '=' | '!' | 'v')
        })
    {
        return None;
    }
    let start = spec.find(|c: char| c.is_ascii_digit())?;
    let run: &str = spec[start..]
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?;
    let version = run.trim_end_matches('.');
    (!version.is_empty()).then(|| version.to_owned())
}

/// The version a PEP 508 requirement asks for, from what follows its
/// name: the extras in brackets and the marker after `;` left aside, so
/// their digits do not count.
fn requirement_version(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(after) => after.split_once(']').map_or("", |(_, rest)| rest),
        None => rest,
    };
    version_of(rest.split(';').next().unwrap_or(""))
}

/// The name a PEP 508 requirement starts with.
fn requirement_name(spec: &str) -> &str {
    spec.trim_start()
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .next()
        .unwrap_or("")
}

/// A Python distribution name as PEP 503 compares them: lower case, runs
/// of `-`, `_` and `.` as one `-`.
fn normalized(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
        } else {
            if separator {
                out.push('-');
                separator = false;
            }
            out.extend(c.to_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn framework(name: &'static str, version: Option<&str>) -> Option<Framework> {
        Some(Framework {
            name,
            version: version.map(str::to_owned),
        })
    }

    fn project(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).expect("write");
        }
        dir
    }

    #[test]
    fn finds_nothing_without_a_manifest() {
        let dir = project(&[("README.md", "hi\n")]);
        assert_eq!(detect(dir.path()), None);
    }

    #[test]
    fn reads_cargo_manifests() {
        let dir = project(&[(
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[dependencies]\nanyhow = \"1\"\nratatui = { version = \"0.30.1\", features = [\"a\"] }\ntokio = \"1\"\n",
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Rust",
                framework: framework("ratatui", Some("0.30.1")),
            }),
            "the list's order decides, not the manifest's"
        );
        let dir = project(&[(
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\"]\n\n[workspace.dependencies]\naxum = { git = \"https://x\" }\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("axum", None),
            "a workspace root, a dependency without a version"
        );
        let dir = project(&[(
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[dependencies]\naxum = { workspace = true }\n\n[workspace]\n\n[workspace.dependencies]\naxum = \"0.8\"\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("axum", Some("0.8")),
            "a package that is the workspace root takes its version"
        );
        let dir = project(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Rust",
                framework: None,
            })
        );
        let dir = project(&[("Cargo.toml", "not = toml = at all\n")]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Rust",
                framework: None,
            }),
            "a manifest that does not parse still names the language"
        );
    }

    #[test]
    fn reads_package_manifests() {
        let dir = project(&[(
            "package.json",
            r#"{"dependencies": {"react": "^19.0.0", "next": "15.2.0"}, "devDependencies": {"typescript": "~5.8"}}"#,
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "TypeScript",
                framework: framework("next", Some("15.2.0")),
            })
        );
        let dir = project(&[
            (
                "package.json",
                r#"{"dependencies": {"express": "workspace:*"}}"#,
            ),
            ("tsconfig.json", "{}"),
        ]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "TypeScript",
                framework: framework("express", None),
            }),
            "tsconfig.json says TypeScript; no version from a workspace spec"
        );
        let dir = project(&[("package.json", r#"{"name": "x"}"#)]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "JavaScript",
                framework: None,
            })
        );
        let dir = project(&[("package.json", "{")]);
        assert_eq!(detect(dir.path()).map(|s| s.language), Some("JavaScript"));
        let dir = project(&[("package.json", "{"), ("tsconfig.json", "{}")]);
        assert_eq!(
            detect(dir.path()).map(|s| s.language),
            Some("TypeScript"),
            "the tsconfig counts when the manifest does not parse"
        );
        let dir = project(&[(
            "package.json",
            "\u{feff}{\"dependencies\": {\"vue\": \"^3.5\"}}",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("vue", Some("3.5")),
            "a byte order mark is skipped"
        );
        let dir = project(&[(
            "package.json",
            r#"{"dependencies": {"react": "npm:@preact/compat@^10.19.3", "express": "git+https://github.com/x/y.git#3f2a1b4c"}}"#,
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("react", None),
            "an alias is not a version"
        );
    }

    #[test]
    fn reads_python_manifests() {
        let dir = project(&[(
            "pyproject.toml",
            "[project]\nname = \"x\"\ndependencies = [\"requests\", \"Django>=4.2,<5\", \"fastapi[standard]==0.115.0\"]\n",
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Python",
                framework: framework("django", Some("4.2")),
            }),
            "PEP 508 names compare normalized"
        );
        let dir = project(&[(
            "pyproject.toml",
            "[project]\ndependencies = [\"Django[argon2,bcrypt]==5.1.2 ; python_version >= '3.10'\"]\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("django", Some("5.1.2")),
            "neither the extras nor the marker give the version"
        );
        let dir = project(&[(
            "pyproject.toml",
            "[project]\ndependencies = [\"flask; python_full_version >= '3.8.1'\", \"django @ https://pypi.internal:8443/x/Django-5.0.tar.gz\"]\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("django", None),
            "a URL is not a version; django comes first in the list"
        );
        let dir = project(&[(
            "pyproject.toml",
            "[tool.poetry.dependencies]\npython = \"^3.12\"\nFastAPI = { version = \"^0.115\", extras = [\"standard\"] }\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("fastapi", Some("0.115")),
            "poetry's table"
        );
        let dir = project(&[(
            "pyproject.toml",
            "[build-system]\nrequires = [\"setuptools\"]\n",
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Python",
                framework: None,
            })
        );
    }

    #[test]
    fn reads_go_manifests() {
        let dir = project(&[(
            "go.mod",
            "module example.com/x\n\ngo 1.22\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.10.0 // indirect\n\tgithub.com/go-chi/chi/v5 v5.1.0\n)\n\nrequire github.com/spf13/cobra v1.8.1\n",
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Go",
                framework: framework("chi", Some("5.1.0")),
            }),
            "an indirect requirement does not count; a major suffix does"
        );
        let dir = project(&[("go.mod", "module example.com/x\n\ngo 1.22\n")]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "Go",
                framework: None,
            })
        );
        let dir = project(&[(
            "go.mod",
            "module example.com/x\r\n\r\nrequire\t(\r\n\tgithub.com/gin-gonic/gin v1.10.0\r\n)\r\n// require github.com/go-chi/chi v5\r\n",
        )]);
        assert_eq!(
            detect(dir.path()).and_then(|s| s.framework),
            framework("gin", Some("1.10.0")),
            "a tab after require, Windows line ends, a commented line"
        );
    }

    #[test]
    fn reads_composer_manifests() {
        let dir = project(&[(
            "composer.json",
            r#"{"require": {"php": "^8.2", "laravel/framework": "^11.0"}, "require-dev": {"slim/slim": "4.*"}}"#,
        )]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "PHP",
                framework: framework("laravel", Some("11.0")),
            }),
            "only require counts"
        );
    }

    #[test]
    fn reads_this_project() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            detect(root),
            Some(Stack {
                language: "Rust",
                framework: framework("ratatui", Some("0.30")),
            })
        );
    }

    #[test]
    fn the_first_manifest_in_the_list_decides() {
        let dir = project(&[
            ("package.json", r#"{"dependencies": {"react": "19"}}"#),
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
        ]);
        assert_eq!(detect(dir.path()).map(|s| s.language), Some("Rust"));
        // The assets' package.json of a Laravel or Django project does
        // not make it a JavaScript one.
        let dir = project(&[
            (
                "package.json",
                r#"{"devDependencies": {"vite": "^6", "tailwindcss": "^4"}}"#,
            ),
            (
                "composer.json",
                r#"{"require": {"laravel/framework": "^11.31"}}"#,
            ),
        ]);
        assert_eq!(
            detect(dir.path()),
            Some(Stack {
                language: "PHP",
                framework: framework("laravel", Some("11.31")),
            })
        );
        let dir = project(&[
            (
                "package.json",
                r#"{"devDependencies": {"tailwindcss": "^3.4"}}"#,
            ),
            (
                "pyproject.toml",
                "[project]\ndependencies = [\"django>=5.0\"]\n",
            ),
        ]);
        assert_eq!(detect(dir.path()).map(|s| s.language), Some("Python"));
    }

    #[test]
    fn versions_are_digits_and_dots() {
        assert_eq!(version_of("^15.2.0").as_deref(), Some("15.2.0"));
        assert_eq!(version_of(">=4.2,<5").as_deref(), Some("4.2"));
        assert_eq!(version_of("~1.10").as_deref(), Some("1.10"));
        assert_eq!(version_of("v1.10.0").as_deref(), Some("1.10.0"));
        assert_eq!(version_of("1.0.0-beta.1").as_deref(), Some("1.0.0"));
        assert_eq!(version_of("0.30.").as_deref(), Some("0.30"));
        assert_eq!(version_of("4.*").as_deref(), Some("4"));
        assert_eq!(version_of("1.x").as_deref(), Some("1"));
        assert_eq!(version_of(">= 1.2, < 2").as_deref(), Some("1.2"));
        assert_eq!(version_of("^18.0.0 || ^19.0.0").as_deref(), Some("18.0.0"));
        assert_eq!(version_of("*"), None);
        assert_eq!(version_of("workspace:*"), None);
        assert_eq!(version_of("latest"), None);
        assert_eq!(version_of("dev-main as 11.0"), None);
        assert_eq!(version_of("file:../vendor/react-19"), None);
        assert_eq!(version_of("github:facebook/react#v18.2.0"), None);
        assert_eq!(version_of("npm:@preact/compat@^10.19.3"), None);
        assert_eq!(version_of("https://x:8443/a-5.0.tar.gz"), None);
        assert_eq!(version_of(""), None);
        assert_eq!(requirement_version(">=4.2,<5").as_deref(), Some("4.2"));
        assert_eq!(requirement_version("[argon2]>=4.2").as_deref(), Some("4.2"));
        assert_eq!(requirement_version(" ; python_version>'3.8'"), None);
        assert_eq!(requirement_version("[a2] ; python_version>'3.8'"), None);
        assert_eq!(requirement_version(" @ git+https://x@5.0"), None);
        assert_eq!(requirement_version(""), None);
    }

    #[test]
    fn python_names_compare_normalized() {
        assert_eq!(normalized("Fast_API"), "fast-api");
        assert_eq!(normalized("zope.interface"), "zope-interface");
        assert_eq!(normalized("a--b__c"), "a-b-c");
        assert_eq!(requirement_name("  django>=4.2"), "django");
        assert_eq!(requirement_name("fastapi[standard]==0.1"), "fastapi");
        assert_eq!(requirement_name("name @ https://x"), "name");
        assert_eq!(requirement_name(""), "");
    }
}
