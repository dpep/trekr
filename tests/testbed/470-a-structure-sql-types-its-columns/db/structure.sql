SET statement_timeout = 0;

CREATE FUNCTION public.touch() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;

CREATE TABLE public.widgets (
    id bigint NOT NULL,
    name character varying(60) NOT NULL,
    tag_ids bigint[] DEFAULT '{}'::bigint[] NOT NULL,
    made_at timestamp(6) without time zone
);

ALTER TABLE ONLY public.widgets
    ADD CONSTRAINT widgets_pkey PRIMARY KEY (id);
