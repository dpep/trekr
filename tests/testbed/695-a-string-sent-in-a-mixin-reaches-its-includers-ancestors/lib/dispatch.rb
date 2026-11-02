module Mixin
  def dispatch(kind)
    send("handle_#{kind}")
  end
end
