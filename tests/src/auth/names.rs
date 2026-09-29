use super::*;

#[test]
fn a_token_name_is_a_lowercase_file_name() {
    for valid in ["github", "netflix-password", "a", "9", "a1-b2"] {
        assert!(
            validate_token_name(valid).is_ok(),
            "{valid} must be accepted"
        );
    }
    for invalid in [
        "",
        "Github",
        "-github",
        "_github",
        "netflix_password",
        "netflix password",
        "../escape",
        "a/b",
    ] {
        assert!(
            validate_token_name(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
}

/// The name is written into the workspace's environment directory, so the rule
/// is the set of names the wrapper inside the workspace will export.
#[test]
fn an_environment_token_name_is_an_uppercase_variable_name() {
    for valid in [
        "NETFLIX_PASSWORD",
        "GTBANK_CARD_NUMBER",
        "ENCLAVE_TOKEN",
        "A",
        "A1_B",
    ] {
        assert!(
            validate_env_token_name(valid).is_ok(),
            "{valid} must be accepted"
        );
    }
    for invalid in [
        "",
        "netflix_password",
        "Netflix_Password",
        "NETFLIX-PASSWORD",
        "1TOKEN",
        // A leading underscore derives a slot beginning with '-', which no token
        // file may be called, so the name could never resolve.
        "_TOKEN",
        "TOKEN ",
        "TOKEN=X",
    ] {
        assert!(
            validate_env_token_name(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
    // The bound keeps the name from building a path longer than the filesystem
    // allows.
    assert!(validate_env_token_name(&format!("A{}", "B".repeat(63))).is_ok());
    assert!(validate_env_token_name(&format!("A{}", "B".repeat(64))).is_err());
}

/// The derivation is the whole reason a name the provider table has never heard
/// of can be stored and injected, so the round trip is what the tests pin.
#[test]
fn an_environment_token_reads_the_slot_its_name_derives() {
    assert_eq!(
        slot_for_env_token("NETFLIX_PASSWORD").expect("derive a slot"),
        "netflix-password"
    );
    assert_eq!(
        slot_for_env_token("GTBANK_CARD_NUMBER").expect("derive a slot"),
        "gtbank-card-number"
    );
    assert_eq!(
        slot_for_env_token("ENCLAVE_TOKEN").expect("derive a slot"),
        "enclave-token"
    );

    // Every derived slot is a name the store accepts, which is what makes the
    // mapping total.
    for env_token in ["NETFLIX_PASSWORD", "A", "A_B_C", "X9"] {
        let slot = slot_for_env_token(env_token).expect("derive a slot");
        assert!(
            validate_token_name(&slot).is_ok(),
            "{env_token} derived the unusable slot {slot}"
        );
    }

    assert!(slot_for_env_token("netflix_password").is_err());
}
