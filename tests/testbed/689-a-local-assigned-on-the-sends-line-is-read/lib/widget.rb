class Widget
  def go(name, val); rule = "handle_#{name}"; public_send(rule, val); end

  def handle_ping(val)
    val
  end
end
