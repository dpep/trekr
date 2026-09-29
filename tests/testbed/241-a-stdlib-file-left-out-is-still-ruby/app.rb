class Report
  def run(data)
    JSON.generate(data).upcase
    JSON.dump_default_options
    Time.now.to_json
  end
end
