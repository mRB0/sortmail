use std::env;
use std::io::{Read, stdin};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use maildir::Maildir;
use toml::Table;
use anyhow::{Context, Result};
use clap::{Parser};

/// Read email message from stdin and deliver it to the correct
/// Maildir based on the supplied filtering config.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// TOML config file
    #[arg(short, long, value_name = "FILE.toml")]
    config: PathBuf,

    /// Process the input but don't actually deliver the message
    #[arg(short = 'n', long = "dry-run")]
    dry_run: bool,

    /// Use an alternate root Maildir (default: $HOME/Maildir)
    #[arg(short = 'M', long = "maildir", value_name = "/path/to/Maildir")]
    override_root_maildir: Option<PathBuf>,

    /// Environment variable that contains the original recipient's email address (default: ORIGINAL_RECIPIENT)
    #[arg(short = 'R', long = "recipient-env", value_name = "ENV")]
    original_recipient_environment_variable: Option<String>
}


/// Load config_file as a TOML file containing a mapping of email
/// addresses to Maildir mailboxes.
///
/// Input file should contain tables with a single `addresses` key
/// containing newline-separated email addresses, like:
///
/// [MailboxName]
/// addresses = """
/// address1@example.com
/// address2@example.com
/// """
///
/// Return the mapping of each email address to the Maildir mailbox
/// name it should be sorted into.
fn load_mappings(config_file: &Path) -> Result<HashMap<String, String>> {
    let contents = std::fs::read_to_string(config_file)
        .with_context(|| format!("Error opening config file {}", config_file.display()))?;

    let document: Table = toml::from_str(&contents)
        .with_context(|| format!("Error parsing config file {}", config_file.display()))?;

    let addresses_to_mailboxes: HashMap<_, _> = document
        .iter()
        .filter_map(
            |(mailbox_name, mailbox_options_value)| {
                let toml::Value::Table(mailbox_options) = mailbox_options_value else {
                    // Skip configuration (any key-value pairs not in a table)
                    return None;
                };

                let addresses_toml = &mailbox_options["addresses"];

                let toml::Value::String(addresses) = addresses_toml else {
                    return Some(Err(Box::new(std::io::Error::from(std::io::ErrorKind::InvalidData)))
                                .with_context(||
                                    format!(
                                        "Expected a string value for addresses in config file {} but found {:?}",
                                        config_file.display(),
                                        addresses_toml
                                    )
                                )
                    );
                };

                let address_to_mailbox: Vec<(String, String)> = addresses.split("\n")
                    .map(|addr| addr.trim())
                    .filter(|addr| !addr.is_empty())
                    .map(|addr| (String::from(addr.to_lowercase()), String::from(mailbox_name)))
                    .collect();

                Some(Ok(address_to_mailbox))
            }
        ).collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(addresses_to_mailboxes)
}


fn get_normalized_original_recipient_email_address(args: &Args) -> Result<String> {
    let env_variable: &str = match args.original_recipient_environment_variable {
        Some(ref name) => name,
        None => "ORIGINAL_RECIPIENT"
    };

    Ok(env::var(env_variable)
       .with_context(|| format!("Missing {} environment variable for recipient email address", env_variable))?
       .to_lowercase()
    )
}


/// Load an email message from stdin and the environment, and deliver
/// it to the right Maildir mailbox based on the mappings detailed in
/// the file at `args.config`.
fn sort_message_from_stdin(args: &Args) -> Result<()> {
    let mut maildir = match args.override_root_maildir {
        Some(ref path) => PathBuf::from(path),
        None => {
            let homedir = env::var("HOME")
                .context("Unable to find HOME environment variable")?;
            let mut path = PathBuf::from(homedir);
            path.push("Maildir");
            path
        }
    };

    let mappings = load_mappings(&args.config)
        .with_context(|| format!("Error loading config file {}", args.config.display()))?;

    // Save to maildir

    let original_recipient_email_address = get_normalized_original_recipient_email_address(args)?;

    if let Some(mailbox_name) = mappings.get(&original_recipient_email_address) {
        maildir.push(format!(".{mailbox_name}"));
    }

    eprintln!(
        "Recipient {original_recipient_email_address}: Deliver to {}{}",
        maildir.display(),
        match args.dry_run {
            true => " (dry run, no actual delivery will be made)",
            false => ""
        }
    );

    let mailbox = Maildir::from(maildir);

    let incoming_message_bytes: Box<[u8]> = stdin()
        .bytes()
        .collect::<Result<_, _>>()
        .context("Error loading message data from stdin")?;

    if incoming_message_bytes.is_empty() {
        return Err(Box::new(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)))
            .context("Empty incoming message data");
    }

    if !args.dry_run {
        mailbox
            .store_new(&incoming_message_bytes)
            .context("Error saving message to Maildir")?;
    }

    Ok(())
}

fn main() {
    let args = Args::parse();

    sort_message_from_stdin(&args).unwrap();
}
