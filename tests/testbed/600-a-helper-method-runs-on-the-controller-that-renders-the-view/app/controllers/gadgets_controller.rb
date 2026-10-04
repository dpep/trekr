class GadgetsController < ApplicationController
  helper_method :label

  def show
  end

  def edit
    render template: "widgets/edit"
  end

  def label
    "gadget"
  end

  def current_account
    :gadget
  end
end
