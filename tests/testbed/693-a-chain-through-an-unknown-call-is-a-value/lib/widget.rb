class Widget
  def self.for(_kind)
    Gadget.new
  end

  def handle_ping
    :ping
  end
end

class Gadget
  def handle_ping
    :ping
  end
end

class Dispatcher
  def run(kind)
    Widget.for(1).public_send("handle_#{kind}")
  end
end
