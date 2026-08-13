use factory_protocol::{
    AggregateRevision, ArtifactId, ContractError, ExpectedRevision, ForumAttachmentInput,
    ForumAttachmentLabel, ForumAuthor, ForumPageLimit, ForumPostBody, ForumPostId, ForumPostInput,
    ForumPostKind, ForumSearchCursor, ForumSearchInput, ForumSearchQuery, ForumThreadId,
    ForumThreadPage, ForumThreadTitle, ForumTopicDescription, ForumTopicName, Office, SessionId,
};

#[test]
fn forum_text_limits_are_utf8_bytes_and_reject_nul() {
    assert!(ForumTopicName::new("x".repeat(160)).is_ok());
    assert!(matches!(
        ForumTopicName::new("x".repeat(161)),
        Err(ContractError::ByteLimitExceeded { .. })
    ));
    // Four-byte code points make the byte boundary observable.
    assert!(ForumPostBody::new("é".repeat(8 * 1024)).is_ok());
    assert!(ForumPostBody::new("é".repeat(8 * 1024 + 1)).is_err());
    assert!(ForumPostBody::new("bad\0body").is_err());
}

#[test]
fn forum_search_cursor_is_deterministic_and_bounded() {
    let post_id = ForumPostId::new(42).unwrap();
    let cursor = ForumSearchCursor::new(0x3f80_0000, post_id);
    let encoded = cursor.encode();
    assert_eq!(encoded, "3f800000.42");
    assert_eq!(ForumSearchCursor::decode(&encoded).unwrap(), cursor);
    assert!(ForumSearchCursor::decode("3f800000").is_err());
    assert!(ForumSearchCursor::decode("3f800000.0").is_err());
    assert!(ForumSearchCursor::decode("7fc00000.42").is_err());
}

#[test]
fn forum_search_range_and_relation_input_are_closed() {
    let query = ForumSearchQuery::new("\"exact phrase\" term").unwrap();
    let mut search = ForumSearchInput::new(query);
    search.created_after_micros = Some(20);
    search.created_before_micros = Some(10);
    assert!(search.validate().is_err());

    let input = ForumPostInput {
        kind: ForumPostKind::Correction,
        body: ForumPostBody::new("corrected").unwrap(),
        reply_to: Some(ForumPostId::new(5).unwrap()),
        supersedes: Some(ForumPostId::new(5).unwrap()),
        attachments: vec![],
    };
    assert!(input.validate().is_err());

    let duplicate = ForumPostInput {
        kind: ForumPostKind::Finding,
        body: ForumPostBody::new("attached").unwrap(),
        reply_to: None,
        supersedes: None,
        attachments: vec![
            ForumAttachmentInput {
                artifact_id: ArtifactId::new(1).unwrap(),
                label: ForumAttachmentLabel::new("evidence").unwrap(),
            },
            ForumAttachmentInput {
                artifact_id: ArtifactId::new(1).unwrap(),
                label: ForumAttachmentLabel::new("same object").unwrap(),
            },
        ],
    };
    assert!(duplicate.validate().is_err());
}

#[test]
fn forum_public_values_keep_identity_and_page_bounds_typed() {
    let actor = ForumAuthor::Actor {
        session_id: SessionId::new(1).unwrap(),
        office: Office::Engineering,
    };
    assert_eq!(
        actor,
        ForumAuthor::Actor {
            session_id: SessionId::new(1).unwrap(),
            office: Office::Engineering,
        }
    );
    assert!(ForumPageLimit::new(0).is_err());
    assert!(ForumPageLimit::new(21).is_err());
    let page = ForumThreadPage::new(
        ForumThreadId::new(7).unwrap(),
        Some(ForumPostId::new(10).unwrap()),
        ForumPageLimit::new(20).unwrap(),
    );
    assert_eq!(page.thread_id.get(), 7);
    assert_eq!(
        ExpectedRevision::new(AggregateRevision::initial())
            .get()
            .get(),
        0
    );
    let _title = ForumThreadTitle::new("thread").unwrap();
    let _description = ForumTopicDescription::new("description").unwrap();
}
