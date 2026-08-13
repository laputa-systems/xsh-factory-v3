use factory_protocol::{
    AssignmentPacketWireV1, JAVASCRIPT_SAFE_INTEGER_MAX, canonical_assignment_packet_json_v1,
    parse_assignment_packet_v1, unsigned_assignment_packet_digest_v1,
};

#[test]
fn print_fixture_unsigned_digest() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json").trim_end();
    let packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    let digest = unsigned_assignment_packet_digest_v1(&packet).expect("unsigned digest");
    println!("{}", digest);
    assert_eq!(
        canonical_assignment_packet_json_v1(&packet).expect("canonical fixture"),
        raw
    );
}

#[test]
fn sealed_fixture_round_trips_and_rejects_noncanonical_bytes() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json").trim_end();
    let packet = parse_assignment_packet_v1(raw.as_bytes()).expect("sealed fixture");
    assert_eq!(
        packet.packet_digest,
        "4ad427f15e5b85976ee0a56303d0565b439a8db379f7859705ed730f1ae39f45"
    );
    assert!(parse_assignment_packet_v1(format!(" {}", raw).as_bytes()).is_err());
    assert!(
        parse_assignment_packet_v1(
            raw.replace(
                "\"packet_digest\":\"4ad427f1",
                "\"unknown\":1,\"packet_digest\":\"4ad427f1"
            )
            .as_bytes()
        )
        .is_err()
    );
}

#[test]
fn canonical_packet_uses_json_unicode_escape_policy() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json");
    let mut packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.packet_digest.clear();
    packet.required_reads[0].reason = "line\u{2028}separator\u{2029}".to_owned();
    let encoded = canonical_assignment_packet_json_v1(&packet).expect("canonical edge packet");
    assert!(encoded.contains("line\u{2028}separator\u{2029}"));
}

#[test]
fn assignment_packet_rejects_unknown_tools_and_model_flags() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json");
    let mut packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.tools.push("actor_invented_tool".to_owned());
    assert!(canonical_assignment_packet_json_v1(&packet).is_err());

    let mut packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet
        .model
        .capability_flags
        .push("unrecognized".to_owned());
    assert!(canonical_assignment_packet_json_v1(&packet).is_err());
}

#[test]
fn assignment_packet_numeric_fields_match_javascript_safe_integer_boundary() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json");
    let mut packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.campaign_id = i64::try_from(JAVASCRIPT_SAFE_INTEGER_MAX).unwrap();
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v1(&packet).is_ok());

    packet.campaign_id = i64::try_from(JAVASCRIPT_SAFE_INTEGER_MAX + 1).unwrap();
    assert!(canonical_assignment_packet_json_v1(&packet).is_err());

    let mut packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    packet.model.price_input_micro_usd_per_million_tokens = JAVASCRIPT_SAFE_INTEGER_MAX + 1;
    packet.packet_digest.clear();
    assert!(canonical_assignment_packet_json_v1(&packet).is_err());
}

#[test]
fn signed_cas_bytes_have_a_distinct_digest_from_the_unsigned_packet_seal() {
    let raw = include_str!("../../../tests/protocol-fixtures/assignment-packet-v1.json");
    let packet: AssignmentPacketWireV1 = miniserde::json::from_str(raw).expect("fixture DTO");
    let unsigned = unsigned_assignment_packet_digest_v1(&packet).expect("unsigned digest");
    let signed = canonical_assignment_packet_json_v1(&packet).expect("canonical signed bytes");
    assert_ne!(
        factory_protocol::ContentDigest::of_bytes(signed.as_bytes()),
        unsigned
    );
}
