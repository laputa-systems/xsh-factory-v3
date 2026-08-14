-- Attach the final measured Factory spend to every immutable local delivery.
-- Existing delivery rows are backfilled from their campaign aggregate before
-- the column becomes mandatory; an unknown historical cost must fail closed.

ALTER TABLE factory.deliveries
    ADD COLUMN factory_cost_micro_usd BIGINT;

DROP TRIGGER deliveries_immutable ON factory.deliveries;

UPDATE factory.deliveries AS delivery
   SET factory_cost_micro_usd = campaign.measured_cost_micro_usd
  FROM factory.candidates AS candidate
  JOIN factory.ticket_attempts AS attempt
    ON attempt.id = candidate.ticket_attempt_id
  JOIN factory.campaigns AS campaign
    ON campaign.id = attempt.campaign_id
 WHERE candidate.id = delivery.candidate_id;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM factory.deliveries
         WHERE factory_cost_micro_usd IS NULL
    ) THEN
        RAISE EXCEPTION
            'cannot attach Factory-Cost to a historical delivery with unknown campaign cost';
    END IF;
END;
$$;

ALTER TABLE factory.deliveries
    ALTER COLUMN factory_cost_micro_usd SET NOT NULL,
    ADD CONSTRAINT deliveries_factory_cost_micro_usd_nonnegative
        CHECK (factory_cost_micro_usd >= 0);

CREATE TRIGGER deliveries_immutable
    BEFORE UPDATE ON factory.deliveries FOR EACH ROW
    EXECUTE FUNCTION factory.reject_delivery_update();

COMMENT ON COLUMN factory.deliveries.factory_cost_micro_usd IS
    'Final known aggregate Factory spend for the campaign that delivered this commit, in micro-USD';
