--
-- PostgreSQL database dump
--

SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);

CREATE TABLE audit.widgets (
    id bigint,
    action text
);

CREATE TABLE public.widgets (
    id bigint NOT NULL,
    name text
);

CREATE VIEW public.recent_widgets AS
 SELECT widgets.id,
    widgets.name,
    (widgets.id * 2)
   FROM public.widgets;

ALTER TABLE ONLY public.widgets
    ADD CONSTRAINT widgets_pkey PRIMARY KEY (id);

SET search_path TO "$user", public;
