class Bus
  def initialize(events)
    @handlers = events.map { |e| method("on_#{e}") }
  end

  def on_start = 1
end
