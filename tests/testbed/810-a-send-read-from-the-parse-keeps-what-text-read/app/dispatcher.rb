class Dispatcher
  def go(x, override)
    m = "handle_#{x}"
    m = override if override
    Widget.public_send(m)
  end
end
