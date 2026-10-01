class Stepper
  def step(name, step_name = :"fetch_#{name}")
    step_name
  end

  def fetch_widget
    :widget
  end
end
