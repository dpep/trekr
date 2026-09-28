class Widget
  def save
    authenticate_widget!
    render_later
  end
end

class Gadget
  def render_later
  end
end
