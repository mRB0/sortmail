use std::env;
use std::io::{Read, stdin};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use maildir::Maildir;
use anyhow::{Context, Result};
use clap::Parser;
use regex::RegexSet;
use serde::{Deserialize, Deserializer};

//
// Command-line args
//

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

    /// Print out the address map before performing delivery
    #[arg(short = 'P', long = "print-address-map")]
    print_address_map: bool,

    /// Use an alternate root Maildir (default: $HOME/Maildir)
    #[arg(short = 'M', long = "maildir", value_name = "/path/to/Maildir")]
    override_root_maildir: Option<PathBuf>,

    /// Environment variable that contains the original recipient's email address (default: ORIGINAL_RECIPIENT)
    #[arg(short = 'R', long = "recipient-env", value_name = "ENV")]
    original_recipient_environment_variable: Option<String>
}

//
// Config file
//

type ConfigToml = HashMap<String, ConfigMailbox>;

#[derive(Deserialize, Debug)]
struct ConfigMailbox {
    #[serde(default, deserialize_with = "deserialize_email_addresses_separated_by_newlines")]
    addresses: Vec<String>,

    #[serde(default, deserialize_with = "deserialize_email_addresses_separated_by_newlines")]
    re_addresses: Vec<String>
}

fn deserialize_email_addresses_separated_by_newlines<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let s = String::deserialize(d)?;

    Ok(s.split("\n")
       .map(|addr| addr.trim().to_lowercase())
       .filter(|addr| !addr.is_empty())
       .collect())
}

//
// Address map
//

#[derive(Debug)]
struct AddressMap {
    exact_address_to_mailbox_name: HashMap<String, Rc<String>>,
    address_regexset_to_mailbox_name: Vec<(RegexSet, Rc<String>)>
}

impl AddressMap {
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
    /// re_addresses = """
    /// ^local_part@
    /// @things.example.com$
    /// """
    ///
    /// Return the mapping of each email address to the Maildir mailbox
    /// name it should be sorted into.
    fn from_file(config_file: &Path) -> Result<AddressMap> {
        let contents = std::fs::read_to_string(config_file)
            .with_context(|| format!("Error opening config file {}", config_file.display()))?;

        let config: ConfigToml = toml::from_str(&contents)
            .with_context(|| format!("Error parsing config file {}", config_file.display()))?;

        let zipped_addresses_result: Result<Vec<_>> = config
            .into_iter()
            .map(|(mailbox_name_string, mailbox_config)| {
                let mailbox_name = Rc::new(mailbox_name_string);

                let exact_address_to_mailbox_name: Vec<(_, _)> = mailbox_config
                    .addresses
                    .into_iter()
                    .map(|address| (address, Rc::clone(&mailbox_name)))
                    .collect();

                let regexset_to_mailbox_name_result = match mailbox_config.re_addresses.is_empty() {
                    true => Ok(None),
                    false => RegexSet::new(mailbox_config.re_addresses)
                        .context("Error parsing regular expressions")
                        .map(|set| Some((set, Rc::clone(&mailbox_name))))
                };

                regexset_to_mailbox_name_result.map(|rstmn| (exact_address_to_mailbox_name, rstmn))

            }).collect();

        let (exact_address_mailbox_name_lists, address_regexset_maybe_mailbox_name): (Vec<_>, Vec<Option<(_, _)>>) = zipped_addresses_result?.into_iter().unzip();

        let exact_address_to_mailbox_name: HashMap<_, _> = exact_address_mailbox_name_lists.into_iter().flatten().collect();
        let address_regexset_to_mailbox_name: Vec<(_, _)> = address_regexset_maybe_mailbox_name.into_iter().flatten().collect();

        Ok(AddressMap {
            exact_address_to_mailbox_name: exact_address_to_mailbox_name,
            address_regexset_to_mailbox_name: address_regexset_to_mailbox_name
        })
    }

    fn mailbox_name_for_address(&self, address: &str) -> Option<&str> {
        if let Some(mailbox_name) = self.exact_address_to_mailbox_name.get(address) {
            return Some(mailbox_name);
        }

        let matching_regex = self.address_regexset_to_mailbox_name.iter().find(
            |(ref re, _)| re.is_match(address)
        );

        matching_regex.map(|item| item.1.as_str())
    }
}

//
// Mailbox delivery
//

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

    let mappings = AddressMap::from_file(&args.config)
        .with_context(|| format!("Error loading config file {}", args.config.display()))?;

    if args.print_address_map {
        dbg!(&mappings);
    }


    // Save to maildir

    let original_recipient_email_address = get_normalized_original_recipient_email_address(args)?;

    if let Some(mailbox_name) = mappings.mailbox_name_for_address(&original_recipient_email_address) {
        maildir.push(format!(".{mailbox_name}"));
    }

    println!(
        "Recipient {original_recipient_email_address}: Deliver to {}{}",
        maildir.display(),
        match args.dry_run {
            true => " (dry run, no actual delivery will be performed)",
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


fn main() -> Result<()> {
    let args = Args::parse();
    sort_message_from_stdin(&args)
}
