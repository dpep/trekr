class Runner
  def run(type)
    "UI::#{type.camelize}".constantize.new
  end
end
