Base.class_eval do
  def wave
    "evaluated-" + super
  end
end
