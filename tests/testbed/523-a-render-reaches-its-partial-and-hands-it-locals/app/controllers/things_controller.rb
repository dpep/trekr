class ThingsController
  def show
    thing = load_thing
    render thing
  end
end
