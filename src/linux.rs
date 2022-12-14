#![cfg(target_os = "linux")]

use regex::Regex;
use std::error::Error;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{file, line};

use clap::ArgMatches;
use simple_error::*;
use simplelog::{trace,debug, info, warn};

use crate::resolve::resolve_versions;
use crate::rversion::*;

use crate::alias::*;
use crate::common::*;
use crate::download::*;
use crate::escalate::*;
use crate::library::*;
use crate::run::*;
use crate::utils::*;

pub const R_ROOT: &str = "/opt/R";
pub const R_VERSIONDIR: &str = "{}";
pub const R_SYSLIBPATH: &str = "{}/lib/R/library";
pub const R_BINPATH: &str = "{}/bin/R";
const R_CUR: &str = "/opt/R/current";

#[cfg(target_arch = "x86_64")]
const UBUNTU_1804_URL: &str = "https://cdn.rstudio.com/r/ubuntu-1804/pkgs/r-{}_1_amd64.deb";
#[cfg(target_arch = "x86_64")]
const UBUNTU_2004_URL: &str = "https://cdn.rstudio.com/r/ubuntu-2004/pkgs/r-{}_1_amd64.deb";
#[cfg(target_arch = "x86_64")]
const UBUNTU_2204_URL: &str = "https://cdn.rstudio.com/r/ubuntu-2204/pkgs/r-{}_1_amd64.deb";
#[cfg(target_arch = "x86_64")]
const DEBIAN_9_URL: &str = "https://cdn.rstudio.com/r/debian-9/pkgs/r-{}_1_amd64.deb";
#[cfg(target_arch = "x86_64")]
const DEBIAN_10_URL: &str = "https://cdn.rstudio.com/r/debian-10/pkgs/r-{}_1_amd64.deb";
#[cfg(target_arch = "x86_64")]
const DEBIAN_11_URL: &str = "https://cdn.rstudio.com/r/debian-11/pkgs/r-{}_1_amd64.deb";

#[cfg(target_arch = "aarch64")]
const UBUNTU_1804_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-ubuntu-1804-{}_1_arm64.deb";
#[cfg(target_arch = "aarch64")]
const UBUNTU_2004_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-ubuntu-2004-{}_1_arm64.deb";
#[cfg(target_arch = "aarch64")]
const UBUNTU_2204_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-ubuntu-2204-{}_1_arm64.deb";
#[cfg(target_arch = "aarch64")]
const DEBIAN_9_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-debian-9-{}_1_arm64.deb";
#[cfg(target_arch = "aarch64")]
const DEBIAN_10_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-debian-10-{}_1_arm64.deb";
#[cfg(target_arch = "aarch64")]
const DEBIAN_11_URL: &str =
    "https://github.com/r-hub/R/releases/download/v{}/R-rstudio-debian-11-{}_1_arm64.deb";

const UBUNTU_1804_RSPM: &str = "https://packagemanager.rstudio.com/all/__linux__/bionic/latest";
const UBUNTU_2004_RSPM: &str = "https://packagemanager.rstudio.com/all/__linux__/focal/latest";
const UBUNTU_2204_RSPM: &str = "https://packagemanager.rstudio.com/all/__linux__/jammy/latest";

pub fn sc_add(args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    escalate("adding new R versions")?;

    // This is needed to fix statix linking on Arm Linux :(
    let uid = nix::unistd::getuid().as_raw();
    if false {
        println!("{}", uid);
    }

    let linux = detect_linux()?;
    let version = get_resolve(args)?;
    let alias = get_alias(args);
    let ver = version.version.to_owned();
    let verstr = match ver {
        Some(ref x) => x,
        None => "???",
    };

    let url: String = match &version.url {
        Some(s) => s.to_string(),
        None => bail!("Cannot find a download url for R version {}", verstr),
    };

    let filename = basename(&url).unwrap_or_else(|| "foo");
    let tmp_dir = std::env::temp_dir().join("rig");
    let target = tmp_dir.join(&filename);
    if target.exists() && not_too_old(&target) {
        info!("{} is cached at {}", filename, target.display());
    } else {
        info!("Downloading {} -> {}", url, target.display());
        let client = &reqwest::Client::new();
        download_file(client, &url, &target.as_os_str())?;
    }

    let dirname;
    if linux.distro == "ubuntu" || linux.distro == "debian" {
        add_deb(&target.as_os_str())?;
        dirname = get_install_dir_deb(&target.as_os_str())?;
    } else {
        bail!("Only Ubuntu and Debian Linux are supported currently");
    }

    set_default_if_none(dirname.to_string())?;

    library_update_rprofile(&dirname.to_string())?;
    sc_system_make_links()?;
    match alias {
        Some(alias) => add_alias(&dirname, &alias)?,
        None => { }
    };

    if !args.is_present("without-cran-mirror") {
        set_cloud_mirror(Some(vec![dirname.to_string()]))?;
    }

    if !args.is_present("without-rspm") {
        set_rspm(Some(vec![dirname.to_string()]), &linux)?;
    }

    if !args.is_present("without-sysreqs") {
        set_sysreqs(Some(vec![dirname.to_string()]), &linux)?;
    }

    if !args.is_present("without-pak") {
        system_add_pak(
            Some(vec![dirname.to_string()]),
            require_with!(args.value_of("pak-version"), "clap error"),
            // If this is specified then we always re-install
            args.occurrences_of("pak-version") > 0,
        )?;
    }

    Ok(())
}

fn get_install_dir_deb(path: &OsStr) -> Result<String, Box<dyn Error>> {
    let path2 = Path::new(path);
    let out = try_with!(
        Command::new("dpkg").arg("-I").arg(path).output(),
        "Failed to run dpkg -I {} @{}:{}",
        path2.display(),
        file!(),
        line!()
    );
    let std = try_with!(
        String::from_utf8(out.stdout),
        "Non-UTF-8 output from dpkg -I {} @{}:{}",
        path2.display(),
        file!(),
        line!()
    );
    let lines = std.lines();
    let re = Regex::new("^[ ]*Package: r-(.*)$")?;
    let lines: Vec<&str> = lines.filter(|l| re.is_match(l)).collect();
    let ver = re.replace(lines[0], "${1}");

    Ok(ver.to_string())
}

fn add_deb(path: &OsStr) -> Result<(), Box<dyn Error>> {
    info!("Running apt-get update");
    let mut args: Vec<OsString> = vec![];
    args.push(os("update"));
    run("apt-get".into(), args, "apt-get update")?;

    info!("Running apt install");
    let mut args: Vec<OsString> = vec![];
    args.push(os("install"));
    args.push(os("--reinstall"));
    args.push(os("-y"));
    // https://askubuntu.com/a/668859
    args.push(os("-o=Dpkg::Use-Pty=0"));
    args.push(path.to_os_string());
    run("apt".into(), args, "apt install")?;

    Ok(())
}

pub fn sc_rm(args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    escalate("removing R versions")?;
    let vers = args.values_of("version");
    if vers.is_none() {
        return Ok(());
    }
    let vers = require_with!(vers, "clap error");

    for ver in vers {
        let ver = check_installed(&ver.to_string())?;

        let pkgname = "r-".to_string() + &ver;
        let out = try_with!(
            Command::new("dpkg").args(["-s", &pkgname]).output(),
            "Failed to run dpkg -s {} @{}:{}",
            pkgname,
            file!(),
            line!()
        );

        if out.status.success() {
            info!("Removing {} package", pkgname);
	    let mut args: Vec<OsString> = vec![];
	    args.push(os("remove"));
	    args.push(os("-y"));
	    // https://askubuntu.com/a/668859
	    args.push(os("-o=Dpkg::Use-Pty=0"));
	    args.push(os("--purge"));
	    args.push(os(&pkgname));
	    run("apt-get".into(), args, "apt-get remove")?;
        } else {
            info!("{} package is not installed", pkgname);
        }

        let dir = Path::new(R_ROOT);
        let dir = dir.join(&ver);
        if dir.exists() {
            info!("Removing {}", dir.display());
            try_with!(
                std::fs::remove_dir_all(&dir),
                "Failed to remove {} @{}:{}",
                dir.display(),
                file!(),
                line!()
            );
        }
    }

    sc_system_make_links()?;

    Ok(())
}

pub fn sc_system_make_links() -> Result<(), Box<dyn Error>> {
    escalate("making R-* quick links")?;
    let vers = sc_get_list()?;
    let base = Path::new(R_ROOT);

    // Create new links
    for ver in vers {
        let linkfile = Path::new("/usr/local/bin").join("R-".to_string() + &ver);
        let target = base.join(&ver).join("bin/R");
        if !linkfile.exists() {
            info!("Adding {} -> {}", linkfile.display(), target.display());
            symlink(&target, &linkfile)?;
        }
    }

    // Remove dangling links
    let paths = std::fs::read_dir("/usr/local/bin")?;
    let re = Regex::new("^R-([0-9]+[.][0-9]+[.][0-9]+|oldrel|next|release|devel)$")?;
    for file in paths {
        let path = file?.path();
        // If no path name, then path ends with ..., so we can skip
        let fnamestr = match path.file_name() {
            Some(x) => x,
            None => continue,
        };
        // If the path is not UTF-8, we'll skip it, this should not happen
        let fnamestr = match fnamestr.to_str() {
            Some(x) => x,
            None => continue,
        };
        if re.is_match(&fnamestr) {
            match std::fs::read_link(&path) {
                Err(_) => warn!("<magenra>[WARN]</> {} is not a symlink", path.display()),
                Ok(target) => {
                    if !target.exists() {
                        info!("Cleaning up {}", target.display());
                        match std::fs::remove_file(&path) {
                            Err(err) => {
                                warn!("Failed to remove {}: {}", path.display(), err.to_string())
                            }
                            _ => {}
                        }
                    }
                }
            };
        }
    }
    Ok(())
}

pub fn re_alias() -> Regex {
    let re= Regex::new("^R-(release|oldrel)$").unwrap();
    re
}

pub fn find_aliases() -> Result<Vec<Alias>, Box<dyn Error>> {
    debug!("Finding existing aliases");

    let paths = std::fs::read_dir("/usr/local/bin")?;
    let re = re_alias();
    let mut result: Vec<Alias> = vec![];

    for file in paths {
        let path = file?.path();
        // If no path name, then path ends with ..., so we can skip
        let fnamestr = match path.file_name() {
            Some(x) => x,
            None => continue,
        };
        // If the path is not UTF-8, we'll skip it, this should not happen
        let fnamestr = match fnamestr.to_str() {
            Some(x) => x,
            None => continue,
        };
        if re.is_match(&fnamestr) {
	    trace!("Checking {}", path.display());
            match std::fs::read_link(&path) {
                Err(_) => debug!("{} is not a symlink", path.display()),
                Ok(target) => {
                    if !target.exists() {
                        debug!("Target does not exist at {}", target.display());

                    } else {
                        let version = version_from_link(target);
                        match version {
                            None => continue,
                            Some(version) => {
				trace!("{} -> {}", fnamestr, version);
                                let als = Alias {
                                    alias: fnamestr[2..].to_string(),
                                    version: version.to_string()
                                };
                                result.push(als);
                            }
                        };
                    }
                }
            };
        }
    }

    Ok(result)
}

fn version_from_link(pb: PathBuf) -> Option<String> {
    let osver = match pb.parent()
        .and_then(|x| x.parent())
        .and_then(|x| x.file_name()) {
        None => None,
        Some(s) => Some(s.to_os_string())
    };

    let s = match osver {
        None => None,
        Some(os) => os.into_string().ok()
    };

    s
}

pub fn get_resolve(args: &ArgMatches) -> Result<Rversion, Box<dyn Error>> {
    let str = args
        .value_of("str")
        .ok_or(SimpleError::new("Internal argument error"))?;

    let eps = vec![str.to_string()];
    let me = detect_linux()?;
    let version = resolve_versions(eps, "linux".to_string(), "default".to_string(), Some(me))?;
    Ok(version[0].to_owned())
}

pub fn sc_get_list() -> Result<Vec<String>, Box<dyn Error>> {
    let mut vers = Vec::new();
    if !Path::new(R_ROOT).exists() {
        return Ok(vers);
    }

    let paths = std::fs::read_dir(R_ROOT)?;

    for de in paths {
        let path = de?.path();
        // If no path name, then path ends with ..., so we can skip
        let fname = match path.file_name() {
            Some(x) => x,
            None => continue,
        };
        // If the path is not UTF-8, we'll skip it, this should not happen
        let fname = match fname.to_str() {
            Some(x) => x,
            None => continue,
        };
        if fname == "current" {
            continue;
        }
        // If there is no bin/R, then this is not an R installation
        let rbin = path.join("bin").join("R");
        if !rbin.exists() {
            continue;
        }

        vers.push(fname.to_string());
    }
    vers.sort();
    Ok(vers)
}

pub fn sc_set_default(ver: &str) -> Result<(), Box<dyn Error>> {
    escalate("setting the default R version")?;
    let ver = check_installed(&ver.to_string())?;

    // Remove current link
    if Path::new(R_CUR).exists() {
        std::fs::remove_file(R_CUR)?;
    }

    // Add current link
    let path = Path::new(R_ROOT).join(ver);
    std::os::unix::fs::symlink(&path, R_CUR)?;

    // Remove /usr/local/bin/R link
    let r = Path::new("/usr/local/bin/R");
    if r.exists() {
        std::fs::remove_file(r)?;
    }

    // Add /usr/local/bin/R link
    let cr = Path::new("/opt/R/current/bin/R");
    std::os::unix::fs::symlink(&cr, &r)?;

    // Remove /usr/local/bin/Rscript link
    let rs = Path::new("/usr/local/bin/Rscript");
    if rs.exists() {
        std::fs::remove_file(rs)?;
    }

    // Add /usr/local/bin/Rscript link
    let crs = Path::new("/opt/R/current/bin/Rscript");
    std::os::unix::fs::symlink(&crs, &rs)?;

    Ok(())
}

pub fn sc_get_default() -> Result<Option<String>, Box<dyn Error>> {
    read_version_link(R_CUR)
}

fn set_cloud_mirror(vers: Option<Vec<String>>) -> Result<(), Box<dyn Error>> {
    let vers = match vers {
        Some(x) => x,
        None => sc_get_list()?,
    };

    info!("Setting default CRAN mirror");

    for ver in vers {
        let ver = check_installed(&ver)?;
        let path = Path::new(R_ROOT).join(ver.as_str());
        let profile = path.join("lib/R/library/base/R/Rprofile".to_string());
        if !profile.exists() {
            continue;
        }

        append_to_file(
            &profile,
            vec!["options(repos = c(CRAN = \"https://cloud.r-project.org\"))".to_string()],
        )?;
    }
    Ok(())
}

fn set_rspm(vers: Option<Vec<String>>, linux: &LinuxVersion) -> Result<(), Box<dyn Error>> {
    let arch = std::env::consts::ARCH;
    if arch != "x86_64" {
        info!("RSPM does not support this architecture: {}", arch);
        return Ok(());
    }

    if !linux.rspm {
        info!(
            "RSPM (or rig) does not support this distro: {} {}",
            linux.distro, linux.version
        );
        return Ok(());
    }

    info!("Setting up RSPM");

    let vers = match vers {
        Some(x) => x,
        None => sc_get_list()?,
    };

    let rcode = r#"
options(repos = c(RSPM="%url%", getOption("repos")))
options(HTTPUserAgent = sprintf("R/%s R (%s)", getRversion(), paste(getRversion(), R.version$platform, R.version$arch, R.version$os)))
"#;

    let rcode = rcode.to_string().replace("%url%", &linux.rspm_url);

    for ver in vers {
        let ver = check_installed(&ver)?;
        let path = Path::new(R_ROOT).join(ver.as_str());
        let profile = path.join("lib/R/library/base/R/Rprofile".to_string());
        if !profile.exists() {
            continue;
        }

        append_to_file(&profile, vec![rcode.to_string()])?;
    }
    Ok(())
}

fn set_sysreqs(vers: Option<Vec<String>>, linux: &LinuxVersion) -> Result<(), Box<dyn Error>> {
    if linux.distro != "ubuntu" || !linux.rspm {
        info!(
            "Skipping optional sysreqs setup, no sysreqs support for this distro: {} {}",
            linux.distro, linux.version
        );
        return Ok(());
    }

    info!("Setting up automatic system requirements installation.");

    let vers = match vers {
        Some(x) => x,
        None => sc_get_list()?,
    };

    let rcode = r#"
Sys.setenv(PKG_SYSREQS = "true")
"#;

    for ver in vers {
        let ver = check_installed(&ver)?;
        let path = Path::new(R_ROOT).join(ver.as_str());
        let profile = path.join("lib/R/library/base/R/Rprofile".to_string());
        if !profile.exists() {
            continue;
        }

        append_to_file(&profile, vec![rcode.to_string()])?;
    }
    Ok(())
}

pub fn sc_system_allow_core_dumps(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Linux
    Ok(())
}

pub fn sc_system_allow_debugger(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Linux
    Ok(())
}

pub fn sc_system_allow_debugger_rstudio(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Linux
    Ok(())
}

pub fn sc_system_make_orthogonal(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Windows
    Ok(())
}

pub fn sc_system_fix_permissions(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Windows
    Ok(())
}

pub fn sc_system_forget() -> Result<(), Box<dyn Error>> {
    // Nothing to do on Windows
    Ok(())
}

pub fn sc_system_no_openmp(_args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    // Nothing to do on Windows
    Ok(())
}

fn detect_linux() -> Result<LinuxVersion, Box<dyn Error>> {
    let release_file = Path::new("/etc/os-release");
    let lines = read_lines(release_file)?;

    let re_id = Regex::new("^ID=")?;
    let wid_line = grep_lines(&re_id, &lines);
    let mut id = if wid_line.len() == 0 {
        "".to_string()
    } else {
        let id_line = &lines[wid_line[0]];
        let id = re_id.replace(&id_line, "").to_string();
        unquote(&id)
    };

    let re_ver = Regex::new("^VERSION_ID=")?;
    let wver_line = grep_lines(&re_ver, &lines);
    let mut ver = if wver_line.len() == 0 {
        "".to_string()
    } else {
        let ver_line = &lines[wver_line[0]];
        let ver = re_ver.replace(&ver_line, "").to_string();
        unquote(&ver)
    };

    let mut mine = LinuxVersion {
        distro: id.to_owned(),
        version: ver.to_owned(),
        url: "".to_string(),
        rspm: false,
        rspm_url: "".to_string(),
    };

    debug!("Detected distro: {} {}", mine.distro, mine.version);

    // Maybe Deepin?
    if id == "Deepin" {
	let debverfile = Path::new("/etc/debian_version");
	let mut ver = "unknown".to_string();
	if debverfile.exists() {
	    let lines = read_lines(debverfile)?;
	    if lines.len() > 0 {
		let re_ver = Regex::new("[.][0-9]+$")?;
		ver = re_ver.replace(&lines[0], "").to_string();
	    }
	}
	mine.distro = "debian".to_string();
	mine.version = ver;
    }

    let supported = list_supported_distros();

    let mut good = false;
    for dis in &supported {
        if dis.distro == mine.distro && dis.version == mine.version {
            mine.url = dis.url.to_owned();
            mine.rspm = dis.rspm.to_owned();
            mine.rspm_url = dis.rspm_url.to_owned();
            good = true;
        }
    }

    // Maybe an Ubuntu-like distro
    if !good {
	debug!("Unsupported distro, checking if an Ubuntu derivative");
        let re_codename = Regex::new("^UBUNTU_CODENAME=")?;
        let codename_line = grep_lines(&re_codename, &lines);
        if codename_line.len() != 0 {
            let codename_line = &lines[codename_line[0]];
            let codename = re_codename.replace(&codename_line, "").to_string();

            (id, ver) = match &codename[..] {
                "bionic" => ("ubuntu".to_string(), "18.04".to_string()),
                "focal" => ("ubuntu".to_string(), "20.04".to_string()),
                "jammy" => ("ubuntu".to_string(), "22.04".to_string()),
                _ => ("".to_string(), "".to_string()),
            };

            mine.distro = id.to_owned();
            mine.version = ver.to_owned();
            for dis in &supported {
                if dis.distro == mine.distro && dis.version == mine.version {
                    mine.url = dis.url.to_owned();
                    mine.rspm = dis.rspm.to_owned();
                    mine.rspm_url = dis.rspm_url.to_owned();
                    good = true;
		    debug!("Distro derivative of {} {}", id, ver);
                }
            }
        }
    }

    if !good {
        bail!(
            "Unsupported distro: {} {}, only {} are supported currently",
            &id,
            &ver,
            "Ubuntu 18.04, 20.04, 22.04 and Debian 9-11"
        );
    }

    Ok(mine)
}

fn list_supported_distros() -> Vec<LinuxVersion> {
    vec![
        LinuxVersion {
            distro: "ubuntu".to_string(),
            version: "18.04".to_string(),
            url: UBUNTU_1804_URL.to_string(),
            rspm: true,
            rspm_url: UBUNTU_1804_RSPM.to_string(),
        },
        LinuxVersion {
            distro: "ubuntu".to_string(),
            version: "20.04".to_string(),
            url: UBUNTU_2004_URL.to_string(),
            rspm: true,
            rspm_url: UBUNTU_2004_RSPM.to_string(),
        },
        LinuxVersion {
            distro: "ubuntu".to_string(),
            version: "22.04".to_string(),
            url: UBUNTU_2204_URL.to_string(),
            rspm: true,
            rspm_url: UBUNTU_2204_RSPM.to_string(),
        },
        LinuxVersion {
            distro: "debian".to_string(),
            version: "9".to_string(),
            url: DEBIAN_9_URL.to_string(),
            rspm: false,
            rspm_url: "".to_string(),
        },
        LinuxVersion {
            distro: "debian".to_string(),
            version: "10".to_string(),
            url: DEBIAN_10_URL.to_string(),
            rspm: false,
            rspm_url: "".to_string(),
        },
        LinuxVersion {
            distro: "debian".to_string(),
            version: "11".to_string(),
            url: DEBIAN_11_URL.to_string(),
            rspm: false,
            rspm_url: "".to_string(),
        },
    ]
}

pub fn sc_clean_registry() -> Result<(), Box<dyn Error>> {
    // Nothing to do on Linux
    Ok(())
}

pub fn sc_system_update_rtools40() -> Result<(), Box<dyn Error>> {
    Ok(())
}

pub fn sc_rstudio_(version: Option<&str>, project: Option<&str>, arg: Option<&OsStr>)
                   -> Result<(), Box<dyn Error>> {
    let (cmd, mut args) = match project {
        Some(p) => ("xdg-open", vec![p]),
        None => ("rstudio", vec![]),
    };

    let mut envname = "dummy";
    let mut path = "".to_string();
    if let Some(ver) = version {
        let ver = check_installed(&ver.to_string())?;
        envname = "RSTUDIO_WHICH_R";
        path = R_ROOT.to_string() + "/" + &ver + "/bin/R"
    };

    if let Some(arg) = arg {
        args.push(arg.to_str().unwrap_or("."));
    }

    info!("Running {} {}", cmd, args.join(" "));

    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(envname, &path)
        .spawn()?;

    Ok(())
}

pub fn get_r_binary(rver: &str) -> Result<PathBuf, Box<dyn Error>> {
    debug!("Finding R binary for R {}", rver);
    let bin = Path::new(R_ROOT).join(rver).join("bin/R");
    debug!("R {} binary is at {}", rver, bin.display());
    Ok(bin)
}

#[allow(dead_code)]
pub fn get_system_renviron(rver: &str) -> Result<PathBuf, Box<dyn Error>> {
    let renviron = Path::new(R_ROOT).join(rver).join("lib/R/etc/Renviron");
    Ok(renviron)
}

pub fn get_system_profile(rver: &str) -> Result<PathBuf, Box<dyn Error>> {
    let profile = Path::new(R_ROOT)
        .join(rver)
        .join("lib/R/library/base/R/Rprofile");
    Ok(profile)
}

pub fn check_has_pak(_rver: &str) -> Result<(), Box<dyn Error>> {
    // TODO: actually check. Right now the install will fail
    Ok(())
}
