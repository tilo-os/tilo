//! A calendar account connected for reading only: its entries are kept and
//! read as any account's are, and nothing is ever written to its calendar
//! server. Not by the holder that keeps it, not by an agent a member's
//! device handed its session to, not through the person's own asking: each
//! is refused in words that say the account is connected for reading only,
//! before anybody is asked anything. Adding the account again lifts nothing.
//! Its calendar server is asked to read and nothing else, and writes to
//! nobody.

use super::*;

/// What every refusal says of such an account.
const READING_ONLY: &str = "connected for reading only";

/// `answer` is a refusal that says the account is connected for reading
/// only.
fn reading_only(what: &str, answer: Result<Value, ActionError>) {
    let reason = refused(what, answer);
    assert!(reason.contains(READING_ONLY), "{what}: {reason}");
}

/// Whether `account`, as a list answers one, is connected for reading only
/// and holds calendars none of which can be written.
fn kept_for_reading(account: &Value) -> bool {
    account["readOnly"] == true
        && account["calendars"]
            .as_array()
            .is_some_and(|all| !all.is_empty() && all.iter().all(|one| one["readOnly"] == true))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_account_connected_for_reading_only_is_never_written_to() {
    // Her account signs in with her address, so her calendar server would
    // write to the people an entry of hers names when one is stored.
    let ada = Ada::signing_in_as(ADDRESS).await;
    let planning = |start: &str| {
        inviting(
            "planning-1@example.org",
            "Planning",
            start,
            "bo@example.test",
            &[ADDRESS],
        )
    };
    let offsite = |title: &str| {
        inviting(
            "offsite-1@example.org",
            title,
            "20261118T100000",
            ADDRESS,
            &["cy@elsewhere.test"],
        )
    };
    // What another client of hers left there: a meeting Bo invited her to,
    // one she invites Cy to, and an entry of her own.
    ada.server
        .put("work", "planning.ics", &planning("20261117T100000"));
    ada.server.put("work", "offsite.ics", &offsite("Offsite"));
    ada.server.put(
        "work",
        "dentist.ics",
        &meeting("dentist-1@example.org", "Dentist", "20261119T090000", ""),
    );
    let before = ada.server.held("work");

    // She connects the account for reading only. Its server lets her write
    // the calendar; it is kept as one that cannot be written.
    let adding = json!({
        "serverUrl": ada.server.url(),
        "username": ada.user,
        "password": PASSWORD,
        "name": "Mine",
        "readOnly": true,
    });
    let added = ada
        .ask(&ada.herself(), held::ADD, adding)
        .await
        .expect("her own server takes her sign-in");
    assert!(
        kept_for_reading(&added),
        "the account and every calendar of it are kept for reading only: {added}"
    );
    let account = added["id"].as_str().expect("its id").to_owned();

    // Her settings say so, beside the account's server and user name: where
    // a device's Calendar reads the account too.
    let settings = ada
        .workspaces
        .workspace_accounts(&ada.workspace_id.to_string())
        .await
        .unwrap()
        .account(&account)
        .expect("the account is kept")
        .config;
    assert_eq!(settings["read_only"], true, "{settings}");
    assert!(
        settings["calendars"]
            .as_array()
            .is_some_and(|all| !all.is_empty() && all.iter().all(|one| one["read_only"] == true)),
        "every calendar of it is kept as one that cannot be written: {settings}"
    );

    // Adding it again lifts nothing, whoever adds it: the agent her session
    // was handed to with a vault item's reference, once she allowed that,
    // and she herself with the password.
    let assistant = ada.agent("assistant");
    let allowing = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(allowing.clone());
    let login = asked(
        &ada.page(),
        &ada.herself(),
        person_vault::target(person_vault::ADD),
        json!({"kind": "login", "belongsTo": [ada.server.url()], "username": ADDRESS, "secret": PASSWORD}),
    )
    .await
    .expect("she keeps her calendar's login");
    let again = ada
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": ada.server.url(), "password": login["reference"]}),
        )
        .await
        .expect("her assistant adds the account again, with a password it never sees");
    assert_eq!(allowing.count(), 1, "the agent's add asked her");
    assert_eq!(again["id"], added["id"], "it is the same account: {again}");
    assert!(
        kept_for_reading(&again),
        "an agent that adds it again does not lift it: {again}"
    );
    let plain = json!({
        "serverUrl": ada.server.url(),
        "username": ada.user,
        "password": PASSWORD,
        "name": "Mine",
    });
    let again = ada
        .ask(&ada.herself(), held::ADD, plain)
        .await
        .expect("she adds the account again");
    assert!(
        kept_for_reading(&again),
        "it stays connected for reading only until she removes it: {again}"
    );

    // She would allow whatever else she is asked: nothing more is to reach
    // her, because reading only refuses first.
    let answering = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(answering.clone());

    // It is read as any account is, by her assistant and by herself.
    assert_eq!(ada.week(&assistant).await.len(), 3);
    assert_eq!(ada.week(&ada.herself()).await.len(), 3);
    let listed = ada
        .ask(&assistant, held::LIST, json!({}))
        .await
        .expect("her assistant lists her calendars");
    assert!(
        kept_for_reading(&listed["accounts"][0]),
        "her assistant is told the account is connected for reading only: {listed}"
    );
    let dentist = entry_titled(&ada, &assistant, "Dentist").await;
    let hers = entry_titled(&ada, &ada.herself(), "Offsite").await;

    // Her assistant: an invitation put in, one changed, one taken out, and
    // an entry filed.
    let lunch = carried(
        "REQUEST",
        &inviting(
            "lunch-1@example.org",
            "Lunch",
            "20261120T120000",
            "bo@example.test",
            &[ADDRESS],
        ),
    );
    reading_only(
        "an invitation her assistant would put in",
        ada.put(&assistant, &lunch).await,
    );
    reading_only(
        "an invitation her assistant would put into the calendar it names",
        ada.ask(
            &assistant,
            held::ADD_INVITATION,
            json!({"ics": lunch, "calendar": "Work"}),
        )
        .await,
    );
    reading_only(
        "an invitation her assistant would change",
        ada.put(
            &assistant,
            &carried("REQUEST", &planning("20261117T150000")),
        )
        .await,
    );
    reading_only(
        "an invitation her assistant would take out",
        ada.put(&assistant, &carried("CANCEL", &planning("20261117T100000")))
            .await,
    );
    let filing = |entry: &str, under: &str| json!({"entry": entry, "categories": [under]});
    reading_only(
        "an entry her assistant would file",
        ada.ask(&assistant, held::SET_CATEGORIES, filing(&dentist, "Health"))
            .await,
    );

    // She herself: an invitation put in, her own entry filed, and what she
    // invites others to filed and changed.
    reading_only(
        "an invitation she puts in herself",
        ada.put(&ada.herself(), &lunch).await,
    );
    reading_only(
        "her own entry, filed by herself",
        ada.ask(
            &ada.herself(),
            held::SET_CATEGORIES,
            filing(&dentist, "Health"),
        )
        .await,
    );
    reading_only(
        "what she invites others to, filed by herself",
        ada.ask(&ada.herself(), held::SET_CATEGORIES, filing(&hers, "Team"))
            .await,
    );
    reading_only(
        "what she invites others to, changed by herself",
        ada.put(
            &ada.herself(),
            &carried("REQUEST", &offsite("Offsite, moved")),
        )
        .await,
    );

    // Asking its server now is a read, and it stays connected for reading.
    let listed = ada
        .ask(&ada.herself(), held::SYNC, json!({}))
        .await
        .expect("she asks her calendar server now");
    assert!(kept_for_reading(&listed["accounts"][0]), "{listed}");

    // Nobody was asked anything, the calendar server was asked to read and
    // nothing else, it holds what it held, and it wrote to nobody.
    assert_eq!(
        answering.count(),
        0,
        "nobody is asked about what is refused"
    );
    let methods: Vec<String> = ada
        .server
        .requests()
        .into_iter()
        .map(|request| request.method)
        .collect();
    let writes = methods
        .iter()
        .filter(|method| !matches!(method.as_str(), "PROPFIND" | "REPORT" | "GET"))
        .count();
    assert_eq!(
        writes, 0,
        "her calendar server was asked for more than a read: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "REPORT"),
        "it was asked, to read: {methods:?}"
    );
    assert_eq!(
        ada.server.held("work"),
        before,
        "her calendar holds what it held"
    );
    ada.nobody_was_mailed("a calendar account connected for reading only");
}
