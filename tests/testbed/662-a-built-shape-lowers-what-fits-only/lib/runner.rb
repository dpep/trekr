class Runner
  def run(type)
    "Jobs::#{type.camelize}".constantize.new
  end
end
