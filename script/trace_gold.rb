# Record what Ruby *actually* dispatched, as a gold set for scoring `--def`.
#
#     cd <a bootable rails app>
#     bin/rails runner /path/to/trekr/script/trace_gold.rb
#
# Every accuracy claim this project has made so far came from a hand audit of a
# sample. This is the other kind of evidence: a TracePoint watches real
# execution and writes down, for each call site, which method Ruby resolved it
# to and where that method is defined. `script/gold.py` then asks trekr the
# same question and scores it against the truth.
#
# Environment:
#   TREKR_GOLD      output path      (default /tmp/trekr-gold.ndjson)
#   TREKR_EXERCISE  a .rb file to run under the trace. Without one, a default
#                   exercise walks the app's own ActiveRecord models — enough
#                   for a first gold set, and the reason pointing this at a
#                   bigger app is only a matter of writing a better exercise.
#   TREKR_MAX       stop after this many distinct call sites (default 20000)

require "json"
require "set"

OUT      = ENV.fetch("TREKR_GOLD", "/tmp/trekr-gold.ndjson")
EXERCISE = ENV["TREKR_EXERCISE"]
MAX      = Integer(ENV.fetch("TREKR_MAX", "20000"))
APP_ROOT = File.realpath(Dir.pwd)
SELF     = File.realpath(__FILE__)

seen  = Set.new
sites = []

# A call site is a (file, line) — Ruby's stack carries no column. The column is
# recovered by finding the method name on that line, and a line where the name
# appears more than once is dropped rather than guessed at: an ambiguous gold
# entry is worse than a missing one.
def column_of(path, line, name)
  source = (@lines ||= {})[path] ||= begin
    File.readlines(path, chomp: true)
  rescue StandardError
    []
  end
  text = source[line - 1] or return nil
  # Operators and `[]` are not written as words; only word-like names can be
  # located this way, so the rest are skipped.
  return nil unless name =~ /\A[a-zA-Z_]\w*[?!=]?\z/

  hits = text.enum_for(:scan, /(?<![\w.:@$])#{Regexp.escape(name)}(?![\w?!:])/)
             .map { Regexp.last_match.begin(0) }
  return nil unless hits.size == 1

  hits.first + 1
end

def scope_of(path)
  return "app" if path.start_with?(APP_ROOT)
  return "gem" if path.include?("/gems/")

  "other"
end

trace = TracePoint.new(:call) do |tp|
  next if sites.size >= MAX

  # The frame that made this call. `caller_locations(0)` inside the handler
  # starts at the handler itself, so walk to the first frame that is neither
  # this script nor the method being entered.
  location = caller_locations(2, 6)&.find { |l| l.path != SELF && l.path != tp.path }
  next unless location

  caller_path = location.absolute_path || location.path
  next if caller_path.nil? || caller_path == SELF
  next if EXERCISE && caller_path == EXERCISE

  key = [caller_path, location.lineno, tp.method_id]
  next unless seen.add?(key)

  # Where Ruby says the method really lives.
  definition =
    begin
      tp.self.method(tp.method_id).source_location
    rescue StandardError
      [tp.path, tp.lineno]
    end
  next unless definition

  column = column_of(caller_path, location.lineno, tp.method_id.to_s)
  next unless column

  sites << {
    "file" => caller_path,
    "line" => location.lineno,
    "col" => column,
    "method" => tp.method_id.to_s,
    # `defined_class` is Ruby's own answer to "whose method is this" — the
    # module the lookup landed in, which is what trekr's `owner` must match.
    "owner" => tp.defined_class.to_s,
    "def_file" => definition[0],
    "def_line" => definition[1],
    "scope" => scope_of(caller_path)
  }
end

trace.enable

if EXERCISE
  load EXERCISE
else
  # Zeitwerk loads lazily, so without this the app has no models yet and the
  # trace watches an empty program.
  Rails.application.eager_load!

  # A default exercise: touch each model the app defines. Deliberately
  # read-mostly and wrapped, so a model that cannot be built does not end the
  # run — a partial gold set is still a gold set.
  ActiveRecord::Base.descendants.each do |model|
    next if model.abstract_class? || model.name.nil?

    begin
      model.column_names
      model.new
      model.all.limit(1).to_a
      model.reflect_on_all_associations.each { |a| model.new.public_send(a.name) rescue nil }
      model.defined_enums.each_key { |e| model.new.public_send("#{e}?") rescue nil }
    rescue StandardError
      next
    end
  end
end

trace.disable

File.open(OUT, "w") { |f| sites.each { |s| f.puts(JSON.generate(s)) } }
counts = sites.group_by { |s| s["scope"] }.transform_values(&:size)
warn "wrote #{sites.size} gold call sites to #{OUT} (#{counts.inspect})"
