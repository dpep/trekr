class GadgetsController < ApplicationController
  before_action -> { check_authorization(Widget) }, except: %i[ping]

  def archive
  end

  def ping
  end
end
