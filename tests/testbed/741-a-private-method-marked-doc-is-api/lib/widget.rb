module Widget
  class Controller
    private

    def verified_request? # :doc:
    end

    # Override to add to the payload.
    #
    # :doc:
    def append_info_to_payload(payload)
    end

    def helper # see :doc: elsewhere
    end

    def _normalize_args(options) # :doc:
    end

    def show_helper(arg = "@#{name}") # :doc:
    end

    protected

    def form_token # :doc:
    end
  end
end
