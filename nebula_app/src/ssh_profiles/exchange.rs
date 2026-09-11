//! Bounded, credential-free CSV exchange with validation before any mutation.

use super::*;

const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMPORT_HOSTS: usize = 10_000;

#[derive(Clone, Debug)]
pub(crate) struct HostRecord {
    pub destination: String,
    pub label: String,
    pub organization: HostOrganization,
}

pub(crate) fn parse_csv(text: &str) -> Result<Vec<HostRecord>, String> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err("Host import exceeds 8 MiB".into());
    }
    let rows = csv_rows(text.trim_start_matches('\u{feff}'))?;
    let Some(header) = rows.first() else {
        return Err("The host file is empty".into());
    };
    let header: Vec<_> = header.iter().map(|name| name.trim().to_ascii_lowercase()).collect();
    let column = |aliases: &[&str]| header.iter().position(|name| aliases.contains(&name.as_str()));
    let destination = column(&["destination", "host", "hostname", "address"])
        .ok_or("The CSV needs a destination or host column")?;
    let label = column(&["label", "name"]);
    let group = column(&["group", "folder"]);
    let tags = column(&["tags"]);
    let notes = column(&["notes", "description"]);
    let username = column(&["username", "user"]);
    let port = column(&["port"]);
    let secrets: Vec<_> = header
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            matches!(name.as_str(), "password" | "privatekey" | "private_key" | "passphrase")
                .then_some(index)
        })
        .collect();
    let mut records = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, row) in rows.iter().enumerate().skip(1) {
        if row.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        let field = |index: Option<usize>| {
            index.and_then(|index| row.get(index)).map(|s| s.trim()).unwrap_or_default()
        };
        if secrets.iter().any(|column| !field(Some(*column)).is_empty()) {
            return Err(format!(
                "Row {} contains credentials; import host metadata only",
                index + 1
            ));
        }
        let mut target = field(Some(destination)).to_owned();
        if !field(username).is_empty() && !target.contains('@') {
            target = format!("{}@{target}", field(username));
        }
        if !field(port).is_empty() {
            let value = field(port)
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("Invalid port on row {}", index + 1))?;
            // An explicit port column must not silently replace a port in the destination.
            let address = target.rsplit('@').next().unwrap_or(&target);
            if address.contains(':') && !(address.starts_with('[') && address.ends_with(']')) {
                return Err(format!("Ambiguous destination and port on row {}", index + 1));
            }
            target.push_str(&format!(":{value}"));
        }
        validate_ssh_destination(&target).map_err(|e| format!("Row {}: {e}", index + 1))?;
        if !seen.insert(target.clone()) {
            return Err(format!("Duplicate destination on row {}: {target}", index + 1));
        }
        let label = field(label).to_owned();
        if label.chars().count() > 256 || label.chars().any(char::is_control) {
            return Err(format!("Invalid host label on row {}", index + 1));
        }
        let organization = HostOrganization::from_inputs(field(group), field(tags), field(notes))
            .map_err(|e| format!("Row {}: {e}", index + 1))?;
        records.push(HostRecord { destination: target, label, organization });
        if records.len() > MAX_IMPORT_HOSTS {
            return Err("Host import exceeds 10000 entries".into());
        }
    }
    if records.is_empty() {
        return Err("The CSV has no host records".into());
    }
    Ok(records)
}

impl SshProfiles {
    pub(crate) fn import_missing(&mut self, records: &[HostRecord]) -> Result<usize, String> {
        let mut updated = self.clone();
        let mut added = 0;
        for record in records {
            validate_ssh_destination(&record.destination)?;
            record.organization.validate()?;
            if updated.contains(&record.destination) {
                continue;
            }
            updated.upsert(SshProfileAuth {
                destination: record.destination.clone(),
                auth: SshAuthMode::Auto,
                private_keys: vec![],
                label: (!record.label.is_empty()).then(|| record.label.clone()),
                icon: None,
                connection: Default::default(),
            });
            updated.set_organization(&record.destination, record.organization.clone())?;
            added += 1;
        }
        *self = updated;
        Ok(added)
    }

    pub(crate) fn export_csv(&self) -> String {
        let mut csv = "destination,label,group,tags,notes\r\n".to_owned();
        for profile in &self.profiles {
            let meta = self.organization(&profile.destination);
            let tags = meta.tags.join(";");
            let fields = [
                &profile.destination,
                profile.label.as_deref().unwrap_or_default(),
                &meta.group,
                &tags,
                &meta.notes,
            ];
            csv.push_str(&fields.into_iter().map(quote_csv).collect::<Vec<_>>().join(","));
            csv.push_str("\r\n");
        }
        csv
    }
}

fn quote_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut value = String::new();
    let mut quoted = false;
    let mut closed = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    value.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                value.push(c);
            }
            continue;
        }
        match c {
            '"' if value.is_empty() && !closed => quoted = true,
            ',' => {
                row.push(std::mem::take(&mut value));
                closed = false;
            },
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut value));
                rows.push(std::mem::take(&mut row));
                closed = false;
                if rows.len() > MAX_IMPORT_HOSTS + 1 {
                    return Err("Too many CSV rows".into());
                }
            },
            _ if closed || c == '"' => return Err("Invalid CSV quoting".into()),
            _ => value.push(c),
        }
    }
    if quoted {
        return Err("Unclosed CSV quote".into());
    }
    if !row.is_empty() || !value.is_empty() || closed {
        row.push(value);
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip_preserves_unicode_quotes_and_multiline_notes_without_secrets() {
        let records = parse_csv("destination,label,group,tags,notes\nroot@a,\"主机,一\",生产,\"db;linux\",\"line 1\nline \"\"2\"\"\"\n").unwrap();
        let mut profiles = SshProfiles::default();
        profiles.import_missing(&records).unwrap();
        let exported = profiles.export_csv();
        let restored = parse_csv(&exported).unwrap();
        assert_eq!(restored[0].label, "主机,一");
        assert_eq!(restored[0].organization.notes, "line 1\nline \"2\"");
        assert!(!exported.contains("password"));
        assert!(!exported.contains("private_keys"));
    }

    #[test]
    fn invalid_batch_never_partially_mutates_profiles() {
        let mut records = parse_csv("host\na\nb\n").unwrap();
        records[1].destination = "bad;command".into();
        let mut profiles = SshProfiles::default();
        assert!(profiles.import_missing(&records).is_err());
        assert_eq!(profiles.destinations().count(), 0);
    }

    #[test]
    fn importing_again_preserves_existing_credentials_and_labels() {
        let records = parse_csv("host,label\na,Original\n").unwrap();
        let mut profiles = SshProfiles::default();
        assert_eq!(profiles.import_missing(&records).unwrap(), 1);
        profiles.profiles[0].private_keys.push("private-key".into());
        let incoming = parse_csv("host,label\na,Replacement\nb,New\n").unwrap();
        assert_eq!(profiles.import_missing(&incoming).unwrap(), 1);
        assert_eq!(profiles.for_destination("a").label.as_deref(), Some("Original"));
        assert_eq!(profiles.for_destination("a").private_keys, vec![PathBuf::from("private-key")]);
        assert!(!profiles.export_csv().contains("private-key"));
    }

    #[test]
    fn rejects_credentials_duplicate_hosts_bad_ports_and_incomplete_quotes() {
        for csv in
            ["host,password\na,secret", "host\na\na", "host,port\na,0", "host\n\"a", "host\n\"a\"x"]
        {
            assert!(parse_csv(csv).is_err(), "{csv}");
        }
    }
}
