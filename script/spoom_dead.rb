# Runs Spoom's dead-code engine on ROOT and prints its candidates as NDJSON:
# {full_name, kind, path, line}. Spoom's CLI prints neither a kind nor
# anything machine-readable, so this mirrors its flow through the Ruby API,
# as sorbet-deadcode's lib/sorbet_deadcode/spoom/runner.rb does: collect
# files, load the plugins the project's Gemfile.lock selects, index, and keep
# what is dead.
#
#   GEM_HOME=<gems with spoom> ruby script/spoom_dead.rb ROOT
#
# Every file under ROOT is indexed (Spoom's references come only from the
# files it indexes); script/dead_compare.py narrows candidates to a scope.
require "spoom"
require "bundler"
require "json"

root = File.expand_path(ARGV.fetch(0))
Dir.chdir(root) do
  context = Spoom::Context.new(root)
  collector = Spoom::FileCollector.new(
    allow_extensions: [".rb", ".erb", ".gemspec"],
    allow_mime_types: ["text/x-ruby", "text/x-ruby-script"],
    exclude_patterns: %w[vendor sorbet tmp log node_modules].map { |d| "#{root}/#{d}/**" },
  )
  collector.visit_paths(["."])
  plugin_classes = begin
    Spoom::Deadcode.plugins_from_gemfile_lock(context)
  rescue StandardError => e
    warn "spoom_dead: no framework plugins (#{e.class}: #{e.message})"
    Set.new
  end
  warn "spoom_dead: #{collector.files.size} files, plugins: #{plugin_classes.map(&:name).sort.join(", ")}"

  model = Spoom::Model.new
  index = Spoom::Deadcode::Index.new(model)
  plugins = plugin_classes.map { |klass| klass.new(index) }
  skipped = 0
  collector.files.sort.each do |file|
    index.index_file(file, plugins: plugins)
  rescue StandardError
    skipped += 1
  end
  warn "spoom_dead: #{skipped} files Spoom could not index" if skipped.positive?
  model.finalize!
  index.apply_plugins!(plugins)
  index.finalize!

  index.definitions.values.flatten.select(&:dead?).each do |d|
    loc = d.location
    puts JSON.generate(full_name: d.full_name, kind: d.kind.to_s,
                       path: loc.file.delete_prefix("./").delete_prefix("#{root}/"), line: loc.start_line)
  end
end
