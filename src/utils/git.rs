use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use failure::{Error, ResultExt};
use git2;

// This is slightly modified code from crates.io
// https://github.com/rust-lang/crates.io/blob/master/src/git.rs

pub fn add_crate(name: &str, version: &str, repo: &git2::Repository, index_data: &str) -> Result<(), Error> {
    let repo_path = repo.workdir().unwrap();
    let dst = index_file(repo_path, name);

    commit_and_push(repo, || {
        // Add the crate to its relevant file
        fs::create_dir_all(dst.parent().unwrap())?;
        let mut prev = String::new();
        if fs::metadata(&dst).is_ok() {
            File::open(&dst).and_then(|mut f| f.read_to_string(&mut prev))?;
        }
        let new = prev + index_data;
        let mut f = File::create(&dst)?;
        f.write_all(new.as_bytes())?;
        f.write_all(b"\n")?;

        Ok((
            format!("Updating crate `{}#{}`", name, version),
            vec![dst.clone()],
        ))
    })
}

fn index_file(base: &Path, name: &str) -> PathBuf {
    let name = name.chars()
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();
    match name.len() {
        1 => base.join("1").join(&name),
        2 => base.join("2").join(&name),
        3 => base.join("3").join(&name[..1]).join(&name),
        _ => base.join(&name[0..2]).join(&name[2..4]).join(&name),
    }
}

/// Commits and pushes to the crates.io index.
///
/// There are currently 2 instances of the crates.io backend running
/// on Heroku, and they race against each other e.g. if 2 pushes occur,
/// then one will succeed while the other will need to be rebased before
/// being pushed.
///
/// A maximum of 20 attempts to commit and push to the index currently
/// accounts for the amount of traffic publishing crates, though this may
/// have to be changed in the future.
///
/// Notes:
/// Currently, this function is called on the HTTP thread and is blocking.
/// Spawning a separate thread for this function means that the request
/// can return without waiting for completion, and other methods of
/// notifying upon completion or error can be used.
pub fn commit_and_push<F>(repo: &git2::Repository, mut f: F) -> Result<(), Error>
    where
        F: FnMut() -> Result<(String, Vec<PathBuf>), Error>,
{
    let repo_path = repo.workdir().unwrap();

    // Attempt to commit in a loop. It's possible that we're going to need to
    // rebase our repository, and after that it's possible that we're going to
    // race to commit the changes. For now we just cap out the maximum number of
    // retries at a fixed number.
    for _ in 0..20 {
        let (msg, dsts) = f()?;

        // git add $file
        let mut index = repo.index()?;
        let mut repo_path = repo_path.iter();

        for dst in dsts {
            let dst = dst.iter()
                .skip_while(|s| Some(*s) == repo_path.next())
                .collect::<PathBuf>();
            index.add_path(&dst)?;
        }

        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        // git commit -m "..."
        let head = repo.head()?;
        let parent = repo.find_commit(head.target().unwrap())?;
        let sig = repo.signature()?;
        repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[&parent])?;

        // git push
        let mut ref_status = None;
        let mut origin = repo.find_remote("origin")?;
        let res = {
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.credentials(credentials);
            callbacks.push_update_reference(|refname, status| {
                assert_eq!(refname, "refs/heads/master");
                ref_status = status.map(|s| s.to_string());
                Ok(())
            });
            let mut opts = git2::PushOptions::new();
            opts.remote_callbacks(callbacks);
            origin.push(&["refs/heads/master"], Some(&mut opts))
        };
        match res {
            Ok(()) if ref_status.is_none() => return Ok(()),
            Ok(()) => println!("failed to push a ref: {:?}", ref_status),
            Err(e) => println!("failure to push: {}", e),
        }

        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(credentials);
        origin.update_tips(
            Some(&mut callbacks),
            true,
            git2::AutotagOption::Unspecified,
            None,
        )?;

        git_reset_origin(repo)?;
    }

    bail!("Too many rebase failures")
}

pub fn git_reset_origin(repo: &git2::Repository) -> Result<(), Error> {
    println!("aaa");
    let mut origin = repo.find_remote("origin")?;

    // Ok, we need to update, so fetch and reset --hard
    origin.fetch(&[], None, None)?;

    // this is original version from crates.io
//    let head = repo.head()?.target().unwrap();
//    let obj = repo.find_object(head, None)?;

    // my version
    let fetch_head = repo.refname_to_id("FETCH_HEAD")?;
    let obj = repo.find_object(fetch_head, None)?;

    repo.reset(&obj, git2::ResetType::Hard, None)?;
    println!("bbb");
    Ok(())
}

pub fn credentials(
    _user: &str,
    _user_from_url: Option<&str>,
    _cred: git2::CredentialType,
) -> Result<git2::Cred, git2::Error> {
    match (env::var("GIT_HTTP_USER"), env::var("GIT_HTTP_PWD")) {
        (Ok(u), Ok(p)) => git2::Cred::userpass_plaintext(&u, &p),
        _ => Err(git2::Error::from_str("no authentication set")),
    }
}

pub fn get_origin_url(repo: &git2::Repository) -> Result<String, Error> {
    repo
        .find_remote("origin")
        .with_context(|_| "could not find remote origin")
        .map_err(Into::into)
        .and_then(|r| r.url().map(|u| u.to_string()).ok_or_else(|| format_err!("could not find url for remote origin")))
}