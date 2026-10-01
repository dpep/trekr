class Base
  def self.before_action(*names, only: nil, on: nil); end
end

class WidgetsController < Base
  before_action :load_widget, only: [:archive], on: :create

  def archive; end

  def lonely; end

  private

  def load_widget; end
end
